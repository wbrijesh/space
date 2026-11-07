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

/// Events a service plugin can observe (`observes`): it is told, and can't change anything.
pub const EVENT_SESSION_CREATED: &str = "session/created";
pub const EVENT_TURN_STARTED: &str = "turn/started";
pub const EVENT_TURN_ENDED: &str = "turn/ended";
pub const EVENT_TOOL_HELD: &str = "tool/held";
pub const EVENT_TOOL_RESULT: &str = "tool/result";
pub const EVENT_THREAD_TITLED: &str = "thread/titled";
pub const EVENTS: &[&str] = &[
    EVENT_SESSION_CREATED,
    EVENT_TURN_STARTED,
    EVENT_TURN_ENDED,
    EVENT_TOOL_HELD,
    EVENT_TOOL_RESULT,
    EVENT_THREAD_TITLED,
];
/// In `uses`: payloads include what the user and the agent wrote (else ids and metadata only).
pub const USE_READ_TRANSCRIPT: &str = "read:transcript";

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
    /// Environment variables to set (see also [`Manifest::env`]).
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
    /// Capabilities the plugin needs another plugin to provide (`sandbox`, `context`,
    /// `catalog:<name>`). It starts once a provider of each runs, and stops when one stops.
    #[serde(default)]
    pub requires: Vec<String>,
    /// A guard Space can't run tools without: it can't be turned off, and while it isn't
    /// running, tool calls wait for it (fail closed).
    #[serde(default)]
    pub required: bool,
    /// Events a service plugin is told about ([`EVENTS`]).
    #[serde(default)]
    pub observes: Vec<String>,
    /// What a service plugin may ask of the host: host services
    /// ([`crate::service::HOST_SERVICES`]) and [`USE_READ_TRANSCRIPT`].
    #[serde(default)]
    pub uses: Vec<String>,
    /// Variables of Space's environment the plugin gets, beyond the few every plugin gets.
    #[serde(default)]
    pub env: Vec<String>,
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
        for cap in &self.requires {
            let known = matches!(cap.as_str(), CAP_SANDBOX | CAP_CONTEXT)
                || cap.strip_prefix(CATALOG_PREFIX).is_some_and(is_kebab_case);
            if !known {
                return Err(format!("`requires` can't name {cap:?} (only `sandbox`, `context` and `catalog:<name>`)"));
            }
            if self.provides(cap) {
                return Err(format!("`requires` names {cap:?}, which the plugin provides itself"));
            }
        }
        if self.is_agent()
            && (self.provides.len() > 1 || !self.hooks.is_empty() || self.required || !self.observes.is_empty() || !self.uses.is_empty())
        {
            return Err("an `agent` plugin provides nothing else and has no hooks, `observes`, `uses` or `required`".into());
        }
        for hook in &self.hooks {
            if hook.event != HOOK_TOOLS_PRE_EXECUTE {
                return Err(format!("unknown hook event {:?}", hook.event));
            }
        }
        if let Some(event) = self.observes.iter().find(|e| !EVENTS.contains(&e.as_str())) {
            return Err(format!("unknown event {event:?} in `observes`"));
        }
        let known_use = |u: &String| u == USE_READ_TRANSCRIPT || crate::service::HOST_SERVICES.contains(&u.as_str());
        if let Some(u) = self.uses.iter().find(|u| !known_use(u)) {
            return Err(format!("unknown host service {u:?} in `uses`"));
        }
        let env_name = |v: &String| {
            v.chars().next().is_some_and(|c| c.is_ascii_uppercase() || c == '_')
                && v.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        };
        if let Some(v) = self.env.iter().find(|v| !env_name(v)) {
            return Err(format!("`env` lists {v:?}, which isn't an environment variable name"));
        }
        Ok(())
    }

    /// Everything the plugin may do, as stable strings, sorted: `provides:<cap>`,
    /// `hook:<event>`, `observes:<event>`, `uses:<service>`, `env:<NAME>` and `required`.
    /// Approving a plugin approves exactly these.
    pub fn permissions(&self) -> Vec<String> {
        let mut all: Vec<String> = self
            .provides
            .iter()
            .map(|c| format!("provides:{c}"))
            .chain(self.hooks.iter().map(|h| format!("hook:{}", h.event)))
            .chain(self.observes.iter().map(|e| format!("observes:{e}")))
            .chain(self.uses.iter().map(|u| format!("uses:{u}")))
            .chain(self.env.iter().map(|v| format!("env:{v}")))
            .chain(self.required.then(|| "required".to_string()))
            .collect();
        all.sort();
        all.dedup();
        all
    }

    /// Whether the manifest lists `what` in `uses`.
    pub fn uses(&self, what: &str) -> bool {
        self.uses.iter().any(|u| u == what)
    }

    /// Whether the plugin observes `event`.
    pub fn observes(&self, event: &str) -> bool {
        self.observes.iter().any(|e| e == event)
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
             hooks=[{event='tools/pre-execute', order=10}]\nrequired=true\n[run]\ncommand='${target}/space-permissions'\n",
            Path::new("m.toml"),
        )
        .unwrap();
        assert!(!m.is_agent() && m.required);
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
            "id='a'\nname='A'\nversion='1'\nprovides=['agent']\nrequired=true\n[run]\ncommand='x'\n",
            Path::new("m.toml"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("required"), "{err}");
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

    #[test]
    fn requires() {
        let parse = |extra: &str| {
            Manifest::parse(&format!("id='a'\nname='A'\nversion='1'\n{extra}\n[run]\ncommand='x'\n"), Path::new("m.toml"))
        };
        let m = parse("provides=['agent']\nrequires=['sandbox', 'catalog:permission-modes']").unwrap();
        assert_eq!(m.requires, ["sandbox", "catalog:permission-modes"]);
        assert!(parse("provides=['context']").unwrap().requires.is_empty());
        let err = parse("provides=['context']\nrequires=['agent']").unwrap_err();
        assert!(err.to_string().contains("can't name \"agent\""), "{err}");
        let err = parse("provides=['sandbox']\nrequires=['sandbox']").unwrap_err();
        assert!(err.to_string().contains("provides itself"), "{err}");
    }

    #[test]
    fn observes_uses_env_and_permissions() {
        let parse = |extra: &str| {
            Manifest::parse(&format!("id='a'\nname='A'\nversion='1'\n{extra}\n[run]\ncommand='x'\n"), Path::new("m.toml"))
        };
        let m = parse(
            "provides=['context']\nobserves=['turn/ended']\nuses=['host/notify', 'read:transcript']\nenv=['SSH_AUTH_SOCK']\n\
             hooks=[{event='tools/pre-execute'}]\nrequired=true",
        )
        .unwrap();
        assert!(m.observes(EVENT_TURN_ENDED) && !m.observes(EVENT_TURN_STARTED));
        assert!(m.uses(USE_READ_TRANSCRIPT) && m.uses("host/notify") && !m.uses("host/open-url"));
        assert_eq!(
            m.permissions(),
            [
                "env:SSH_AUTH_SOCK",
                "hook:tools/pre-execute",
                "observes:turn/ended",
                "provides:context",
                "required",
                "uses:host/notify",
                "uses:read:transcript"
            ]
        );
        assert!(parse("provides=['context']\nobserves=['turn/over']").unwrap_err().to_string().contains("turn/over"));
        assert!(parse("provides=['context']\nuses=['host/shell']").unwrap_err().to_string().contains("host/shell"));
        assert!(parse("provides=['context']\nenv=['lower']").unwrap_err().to_string().contains("lower"));
        let err = parse("provides=['agent']\nuses=['host/notify']").unwrap_err();
        assert!(err.to_string().contains("nothing else"), "{err}");
        assert_eq!(parse("provides=['agent']\nenv=['SPACE_PI_MODEL']").unwrap().permissions(), ["env:SPACE_PI_MODEL", "provides:agent"]);
    }
}
