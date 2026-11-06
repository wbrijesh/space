//! The `space-plugin.toml` plugin manifest.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// File name of the manifest inside a plugin directory.
pub const MANIFEST_FILE: &str = "space-plugin.toml";

/// What a plugin provides. v0 only runs `agent` plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Agent,
    Tool,
    Source,
    Knowledge,
}

impl PluginKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PluginKind::Agent => "agent",
            PluginKind::Tool => "tool",
            PluginKind::Source => "source",
            PluginKind::Knowledge => "knowledge",
        }
    }
}

impl std::fmt::Display for PluginKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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
    pub kind: PluginKind,
    #[serde(default)]
    pub description: String,
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
        Ok(())
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
kind = "agent"
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
        assert_eq!(m.kind, PluginKind::Agent);
        assert_eq!(m.run.args, vec!["dist/main.js"]);
        assert_eq!(m.run.cwd, RunCwd::Plugin);
        assert_eq!(m.run.env.get("FOO").map(String::as_str), Some("bar"));
    }

    #[test]
    fn defaults() {
        let m = Manifest::parse(
            "id='a'\nname='A'\nversion='1'\nkind='tool'\n[run]\ncommand='x'\n",
            Path::new("x.toml"),
        )
        .unwrap();
        assert_eq!(m.run.cwd, RunCwd::Plugin);
        assert!(m.run.args.is_empty());
        assert_eq!(m.description, "");
    }

    #[test]
    fn rejects_bad_kind() {
        let err = Manifest::parse(
            "id='a'\nname='A'\nversion='1'\nkind='robot'\n[run]\ncommand='x'\n",
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
            "id='Pi_Agent'\nname='A'\nversion='1'\nkind='agent'\n[run]\ncommand='x'\n",
            Path::new("m.toml"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("kebab-case"));

        let err = Manifest::parse("id='a'\nname='A'\nversion='1'\nkind='agent'\n", Path::new("m.toml"))
            .unwrap_err();
        assert!(err.to_string().contains("run"), "{err}");
    }

    #[test]
    fn rejects_unknown_fields() {
        let err = Manifest::parse(
            "id='a'\nname='A'\nversion='1'\nkind='agent'\ncolour='red'\n[run]\ncommand='x'\n",
            Path::new("m.toml"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("colour"), "{err}");
    }
}
