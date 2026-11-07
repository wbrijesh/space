//! The `space-plugin.toml` plugin manifest.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// File name of the manifest inside a plugin directory.
pub const MANIFEST_FILE: &str = "space-plugin.toml";

/// Capability of an ACP agent plugin. A plugin that provides it provides nothing else.
pub const CAP_AGENT: &str = "agent";
/// Wraps spawned commands (`sandbox/confine`). One provider at a time.
pub const CAP_SANDBOX: &str = "sandbox";
/// Contributes hidden context sections to each prompt (`context/get`).
pub const CAP_CONTEXT: &str = "context";
/// Declares settings the host shows and stores (`settings/schema`, `settings/changed`).
pub const CAP_SETTINGS: &str = "settings";
/// Prefix of catalog capabilities: `catalog:<name>` publishes the named list (`catalog/get`).
pub const CATALOG_PREFIX: &str = "catalog:";
/// The catalog of permission modes the composer's mode menu shows.
pub const CATALOG_PERMISSION_MODES: &str = "permission-modes";
/// Hook event: before an agent runs a tool (`hooks/tools.preExecute`).
pub const HOOK_TOOLS_PRE_EXECUTE: &str = "tools/pre-execute";

/// `[[hooks]]`: an event the plugin listens to, and its place in the chain (lower runs first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HookSpec {
    pub event: String,
    #[serde(default)]
    pub order: i32,
}

/// Working directory the plugin process is started in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RunCwd {
    /// The plugin's own directory.
    #[default]
    Plugin,
    /// The project (session working) directory.
    Project,
}

/// `[run]`: how to launch the plugin process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    /// Executable; resolved on `PATH` unless it contains a `/`.
    pub command: String,
    /// Arguments. Relative paths that exist inside the plugin dir are resolved against it.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: RunCwd,
    /// Extra environment variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// A parsed `space-plugin.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Unique kebab-case id.
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Capabilities: `agent` (an ACP agent), or any of `sandbox`, `context`, `settings` and
    /// `catalog:<name>` (a service plugin, spoken to over plain JSON-RPC).
    pub provides: Vec<String>,
    /// Events a service plugin listens to.
    #[serde(default)]
    pub hooks: Vec<HookSpec>,
    pub run: RunConfig,
}

/// Manifest loading/validation failure.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("invalid {path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

impl Manifest {
    /// Parse and validate manifest text. `path` is only used for error messages.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ManifestError> {
        let manifest: Manifest = toml::from_str(text).map_err(|e| ManifestError::Parse {
            path: path.to_path_buf(),
            message: e.to_string().trim_end().to_string(),
        })?;
        manifest.validate().map_err(|message| ManifestError::Invalid {
            path: path.to_path_buf(),
            message,
        })?;
        Ok(manifest)
    }

    /// Read and validate `<dir>/space-plugin.toml`.
    pub fn load_from_dir(dir: &Path) -> Result<Self, ManifestError> {
        let path = dir.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&path).map_err(|source| ManifestError::Io {
            path: path.clone(),
            source,
        })?;
        Self::parse(&text, &path)
    }

    /// Semantic checks beyond what serde enforces.
    pub fn validate(&self) -> Result<(), String> {
        if !is_kebab_case(&self.id) {
            return Err(format!(
                "`id` must be kebab-case (lowercase letters, digits and single dashes), got {:?}",
                self.id
            ));
        }
        if self.name.trim().is_empty() {
            return Err("`name` must not be empty".into());
        }
        if self.version.trim().is_empty() {
            return Err("`version` must not be empty".into());
        }
        if self.run.command.trim().is_empty() {
            return Err("`run.command` must not be empty".into());
        }
        if self.provides.is_empty() {
            return Err("`provides` must name at least one capability".into());
        }
        for cap in &self.provides {
            let known = matches!(cap.as_str(), CAP_AGENT | CAP_SANDBOX | CAP_CONTEXT | CAP_SETTINGS)
                || cap.strip_prefix(CATALOG_PREFIX).is_some_and(is_kebab_case);
            if !known {
                return Err(format!("unknown capability {cap:?} in `provides`"));
            }
        }
        if self.is_agent() && (self.provides.len() > 1 || !self.hooks.is_empty()) {
            return Err("an `agent` plugin provides nothing else and has no hooks".into());
        }
        for hook in &self.hooks {
            if hook.event != HOOK_TOOLS_PRE_EXECUTE {
                return Err(format!("unknown hook event {:?}", hook.event));
            }
        }
        Ok(())
    }

    /// An ACP agent plugin (else a service plugin).
    pub fn is_agent(&self) -> bool {
        self.provides.iter().any(|c| c == CAP_AGENT)
    }

    pub fn provides(&self, capability: &str) -> bool {
        self.provides.iter().any(|c| c == capability)
    }

    /// Names of the catalogs the plugin publishes.
    pub fn catalogs(&self) -> impl Iterator<Item = &str> {
        self.provides.iter().filter_map(|c| c.strip_prefix(CATALOG_PREFIX))
    }

    /// The plugin's position in `event`'s chain, if it listens to it.
    pub fn hook_order(&self, event: &str) -> Option<i32> {
        self.hooks.iter().find(|h| h.event == event).map(|h| h.order)
    }

    /// "agent" or "service", for logs.
    pub fn role(&self) -> &'static str {
        if self.is_agent() { "agent" } else { "service" }
    }
}

fn is_kebab_case(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
id = "pi-agent"
name = "Pi Agent"
version = "0.1.0"
provides = ["agent"]
description = "Coding agent."

[run]
command = "node"
args = ["dist/main.js"]
cwd = "plugin"

[run.env]
FOO = "bar"
"#;

    #[test]
    fn parses_doc_example() {
        let m = Manifest::parse(EXAMPLE, Path::new("x.toml")).unwrap();
        assert_eq!(m.id, "pi-agent");
        assert!(m.is_agent());
        assert_eq!(m.run.args, vec!["dist/main.js"]);
        assert_eq!(m.run.cwd, RunCwd::Plugin);
        assert_eq!(m.run.env.get("FOO").map(String::as_str), Some("bar"));
    }

    #[test]
    fn defaults() {
        let m = Manifest::parse(
            "id='a'\nname='A'\nversion='1'\nprovides=['context']\n[run]\ncommand='x'\n",
            Path::new("x.toml"),
        )
        .unwrap();
        assert_eq!(m.run.cwd, RunCwd::Plugin);
        assert!(m.run.args.is_empty());
        assert_eq!(m.description, "");
    }

    #[test]
    fn rejects_bad_capability() {
        let err = Manifest::parse(
            "id='a'\nname='A'\nversion='1'\nprovides=['robot']\n[run]\ncommand='x'\n",
            Path::new("p/space-plugin.toml"),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("p/space-plugin.toml"), "{msg}");
        assert!(msg.contains("robot"), "{msg}");
    }

    #[test]
    fn rejects_bad_id_and_missing_run() {
        let err = Manifest::parse(
            "id='Pi_Agent'\nname='A'\nversion='1'\nprovides=['agent']\n[run]\ncommand='x'\n",
            Path::new("m.toml"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("kebab-case"));

        let err = Manifest::parse("id='a'\nname='A'\nversion='1'\nprovides=['agent']\n", Path::new("m.toml"))
            .unwrap_err();
        assert!(err.to_string().contains("run"), "{err}");
    }

    #[test]
    fn rejects_unknown_fields() {
        let err = Manifest::parse(
            "id='a'\nname='A'\nversion='1'\nprovides=['agent']\ncolour='red'\n[run]\ncommand='x'\n",
            Path::new("m.toml"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("colour"), "{err}");
    }

    #[test]
    fn service_plugin_with_hooks_and_catalogs() {
        let m = Manifest::parse(
            "id='permissions'\nname='P'\nversion='1'\nprovides=['sandbox','context','settings','catalog:permission-modes']\n\
             hooks=[{event='tools/pre-execute', order=10}]\n[run]\ncommand='${target}/space-permissions'\n",
            Path::new("m.toml"),
        )
        .unwrap();
        assert!(!m.is_agent());
        assert!(m.provides(CAP_SANDBOX) && !m.provides(CAP_AGENT));
        assert_eq!(m.catalogs().collect::<Vec<_>>(), [CATALOG_PERMISSION_MODES]);
        assert_eq!(m.hook_order(HOOK_TOOLS_PRE_EXECUTE), Some(10));
        assert_eq!(m.hook_order("other"), None);
    }

    #[test]
    fn rejects_mixed_agent_and_unknown_hook() {
        let err = Manifest::parse(
            "id='a'\nname='A'\nversion='1'\nprovides=['agent','sandbox']\n[run]\ncommand='x'\n",
            Path::new("m.toml"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("nothing else"), "{err}");
        let err = Manifest::parse(
            "id='a'\nname='A'\nversion='1'\nprovides=['context']\nhooks=[{event='tools/post'}]\n[run]\ncommand='x'\n",
            Path::new("m.toml"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("tools/post"), "{err}");
        let err = Manifest::parse("id='a'\nname='A'\nversion='1'\nprovides=[]\n[run]\ncommand='x'\n", Path::new("m.toml"))
            .unwrap_err();
        assert!(err.to_string().contains("at least one"), "{err}");
    }
}
