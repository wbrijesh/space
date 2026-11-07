//! Read Only's macOS sandbox: every command runs under `sandbox-exec` with a profile that
//! allows everything except writing files, apart from the temporary folder and the standard
//! device files. The kernel enforces it, whatever the command (`rm`, `python`, `make`, `git`).

use std::path::PathBuf;

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// The temporary folder commands may still write to: `$TMPDIR`, resolved (`/var` is a symlink
/// to `/private/var`, and the sandbox matches real paths).
pub fn tmpdir() -> PathBuf {
    let dir = std::env::temp_dir();
    dir.canonicalize().unwrap_or(dir)
}

/// SBPL string literal.
fn quote(path: &str) -> String {
    format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The Read Only profile.
pub fn read_only_profile(tmpdir: &str) -> String {
    [
        "(version 1)".to_string(),
        "(allow default)".to_string(),
        "(deny file-write*)".to_string(),
        // Output to the terminal and the bit bucket isn't a change.
        "(allow file-write* (literal \"/dev/null\") (literal \"/dev/tty\") (literal \"/dev/stdout\") \
         (literal \"/dev/stderr\") (regex #\"^/dev/fd/\"))"
            .to_string(),
        format!("(allow file-write* (subpath {}))", quote(tmpdir)),
    ]
    .join(" ")
}

/// `argv` wrapped for Read Only.
pub fn confine_read_only(argv: Vec<String>, tmpdir: &str) -> Vec<String> {
    let mut wrapped = vec![SANDBOX_EXEC.to_string(), "-p".to_string(), read_only_profile(tmpdir)];
    wrapped.extend(argv);
    wrapped
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn run(argv: &[String]) -> (bool, String) {
        let out = Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }

    fn sh(script: &str) -> Vec<String> {
        confine_read_only(vec!["/bin/bash".into(), "-c".into(), script.into()], &tmpdir().display().to_string())
    }

    #[test]
    fn read_only_blocks_writes_but_not_reading_or_tmpdir() {
        let tmp = tmpdir();
        // Outside TMPDIR: /private/tmp (not $TMPDIR on macOS) and the home folder.
        let probe = format!("/private/tmp/space-permissions-probe-{}", std::process::id());
        let home_probe = format!("{}/.space-permissions-probe-{}", std::env::var("HOME").unwrap(), std::process::id());
        for target in [&probe, &home_probe] {
            let (ok, out) = run(&sh(&format!("echo x > {target}")));
            assert!(!ok && out.contains("Operation not permitted"), "{target}: {out}");
            let (ok, out) = run(&sh(&format!("/usr/bin/python3 -c \"open('{target}','w')\" || exit 1")));
            assert!(!ok, "python wrote {target}: {out}");
            let (ok, _) = run(&sh(&format!("touch {target}")));
            assert!(!ok);
            assert!(!std::path::Path::new(target).exists(), "{target} was created");
        }
        // Inside TMPDIR is allowed.
        let inside = tmp.join(format!("space-permissions-ok-{}", std::process::id()));
        let (ok, out) = run(&sh(&format!("echo x > {} && cat {0}", inside.display())));
        assert!(ok && out.trim() == "x", "{out}");
        let _ = std::fs::remove_file(&inside);
        // Reading and inspecting work; output to /dev/null and stderr too.
        let repo = env!("CARGO_MANIFEST_DIR");
        let (ok, out) = run(&sh(&format!("cd {repo} && git log --oneline -1 >/dev/null && ls Cargo.toml && echo err >&2")));
        assert!(ok, "{out}");
        assert!(out.contains("Cargo.toml") && out.contains("err"), "{out}");
    }

    #[test]
    fn profile_quotes_paths() {
        let p = read_only_profile("/a \"b\"");
        assert!(p.contains("(subpath \"/a \\\"b\\\"\")"), "{p}");
        let argv = confine_read_only(vec!["ls".into()], "/t");
        assert_eq!(argv[0], SANDBOX_EXEC);
        assert_eq!(argv.last().unwrap(), "ls");
    }
}
