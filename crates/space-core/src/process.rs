//! Plugin process supervisor: spawn, capture stderr, report exit, kill on drop.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use space_protocol::RunCwd;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::oneshot;

use crate::discovery::DiscoveredPlugin;

/// Bounded buffer of a plugin's stderr lines.
#[derive(Clone, Debug)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<String>>>,
    cap: usize,
}

impl LogBuffer {
    pub fn new(cap: usize) -> Self {
        Self { lines: Arc::new(Mutex::new(VecDeque::with_capacity(cap.min(256)))), cap }
    }

    pub fn push(&self, line: String) {
        let mut lines = self.lines.lock().unwrap();
        if lines.len() == self.cap {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().iter().cloned().collect()
    }

    pub fn tail(&self, n: usize) -> Vec<String> {
        let lines = self.lines.lock().unwrap();
        lines.iter().skip(lines.len().saturating_sub(n)).cloned().collect()
    }
}

/// How a plugin process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitKind {
    /// Stopped by the harness.
    Killed,
    /// Exited by itself with this code (`None` = killed by a signal).
    Exited(Option<i32>),
}

/// A running plugin process. Dropping it kills the process.
#[derive(Debug)]
pub struct PluginProcess {
    pub pid: Option<u32>,
    pub logs: LogBuffer,
    kill_tx: Mutex<Option<oneshot::Sender<()>>>,
}

impl PluginProcess {
    /// Ask the supervisor to kill the process (idempotent).
    pub fn kill(&self) {
        if let Some(tx) = self.kill_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

pub struct Spawned {
    pub process: PluginProcess,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
}

/// Spawn `plugin` per its `[run]` table. Must be called inside a tokio runtime.
///
/// * `on_log` is called for every stderr line.
/// * `on_exit` is called once when the process ends.
pub fn spawn(
    plugin: &DiscoveredPlugin,
    project_dir: &Path,
    on_log: impl Fn(String) + Send + 'static,
    on_exit: impl FnOnce(ExitKind, Vec<String>) + Send + 'static,
) -> Result<Spawned> {
    let run = &plugin.manifest.run;
    let program = resolve_program(&run.command, &plugin.dir)?;
    let mut args = Vec::with_capacity(run.args.len());
    for arg in &run.args {
        args.push(resolve_arg(arg, &plugin.dir)?);
    }
    let cwd = match run.cwd {
        RunCwd::Plugin => plugin.dir.clone(),
        RunCwd::Project => project_dir.to_path_buf(),
    };

    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .current_dir(&cwd)
        .envs(&run.env)
        .env("SPACE_PLUGIN_DIR", &plugin.dir)
        .env("SPACE_PROJECT_DIR", project_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().with_context(|| {
        format!("failed to start plugin `{}`: {} {}", plugin.id(), program.display(), args.join(" "))
    })?;

    let stdin = child.stdin.take().context("plugin stdin unavailable")?;
    let stdout = child.stdout.take().context("plugin stdout unavailable")?;
    let stderr = child.stderr.take().context("plugin stderr unavailable")?;
    let logs = LogBuffer::new(2000);

    let stderr_done = {
        let logs = logs.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                logs.push(line.clone());
                on_log(line);
            }
        })
    };

    let (kill_tx, kill_rx) = oneshot::channel::<()>();
    let pid = child.id();
    {
        let logs = logs.clone();
        tokio::spawn(async move {
            let kind = tokio::select! {
                status = child.wait() => ExitKind::Exited(status.ok().and_then(|s| s.code())),
                _ = kill_rx => {
                    let _ = child.kill().await;
                    ExitKind::Killed
                }
            };
            // Let the stderr reader drain so the last lines are available.
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), stderr_done).await;
            on_exit(kind, logs.tail(20));
        });
    }

    Ok(Spawned { process: PluginProcess { pid, logs, kill_tx: Mutex::new(Some(kill_tx)) }, stdin, stdout })
}

/// Placeholder in `run.command` for the cargo target profile directory Space runs from
/// (`target/debug`), so bundled Rust plugins can point at their binary.
pub const TARGET_PLACEHOLDER: &str = "${target}";
/// Env var overriding what `${target}` expands to.
pub const TARGET_DIR_ENV: &str = "SPACE_TARGET_DIR";

/// What `${target}` expands to: `SPACE_TARGET_DIR`, else the `target/<profile>` directory
/// above this executable (also inside `Space.app`), else this repo's `target/debug`.
pub fn target_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(TARGET_DIR_ENV).filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let from_exe = std::env::current_exe().ok().and_then(|exe| {
        exe.ancestors()
            .find(|d| d.parent().and_then(Path::file_name).is_some_and(|n| n == "target"))
            .map(Path::to_path_buf)
    });
    from_exe.unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug"))
}

/// Resolve the command: paths with a `/` are relative to the plugin dir (`${target}`
/// expands first); bare names are looked up on `PATH` plus common install locations (GUI apps
/// get a minimal `PATH`).
fn resolve_program(command: &str, plugin_dir: &Path) -> Result<PathBuf> {
    if command.contains(TARGET_PLACEHOLDER) {
        let p = PathBuf::from(command.replace(TARGET_PLACEHOLDER, &target_dir().display().to_string()));
        if !p.is_file() {
            bail!("plugin command not found: {} (is it built? `cargo build --workspace`)", p.display());
        }
        return Ok(p);
    }
    if command.contains('/') {
        let p = plugin_dir.join(command);
        if !p.exists() {
            bail!("plugin command not found: {}", p.display());
        }
        return Ok(p);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let extra = ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from);
    std::env::split_paths(&path)
        .chain(extra)
        .map(|d| d.join(command))
        .find(|p| p.is_file())
        .with_context(|| format!("plugin command `{command}` not found on PATH"))
}

/// Relative paths that look like files (contain `/`) resolve against the plugin dir and
/// must exist — this catches unbuilt plugins (e.g. missing `dist/main.js`) with a clear error.
fn resolve_arg(arg: &str, plugin_dir: &Path) -> Result<String> {
    if arg.starts_with('-') || !arg.contains('/') || Path::new(arg).is_absolute() || arg.contains("://") {
        return Ok(arg.to_string());
    }
    let p = plugin_dir.join(arg);
    if !p.exists() {
        bail!("{} does not exist (is the plugin built?)", p.display());
    }
    Ok(p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_buffer_is_bounded() {
        let b = LogBuffer::new(3);
        for i in 0..5 {
            b.push(i.to_string());
        }
        assert_eq!(b.snapshot(), ["2", "3", "4"]);
        assert_eq!(b.tail(2), ["3", "4"]);
    }

    #[test]
    fn resolves_args() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("dist")).unwrap();
        std::fs::write(tmp.path().join("dist/main.js"), "").unwrap();
        assert!(resolve_arg("dist/main.js", tmp.path()).unwrap().ends_with("dist/main.js"));
        assert_eq!(resolve_arg("--flag", tmp.path()).unwrap(), "--flag");
        let err = resolve_arg("dist/missing.js", tmp.path()).unwrap_err();
        assert!(err.to_string().contains("is the plugin built"));
        assert!(resolve_program("definitely-not-a-command-xyz", tmp.path()).is_err());
        assert!(resolve_program("sh", tmp.path()).is_ok());
        // ${target} expands to the profile dir this test binary runs from.
        let stub = resolve_program("${target}/space-stub-agent", tmp.path());
        let target = target_dir();
        assert!(target.ends_with("debug") || target.ends_with("release"), "{}", target.display());
        if target.join("space-stub-agent").is_file() {
            assert_eq!(stub.unwrap(), target.join("space-stub-agent"));
        }
        let err = resolve_program("${target}/no-such-plugin", tmp.path()).unwrap_err();
        assert!(err.to_string().contains("is it built"), "{err}");
    }
}
