//! Which plugins you approved: plugins from `~/.space/plugins` and a project's
//! `.space/plugins` don't start until you approve what their manifest lets them do. Bundled
//! plugins are trusted.
//!
//! Kept as JSON (`~/.space/plugin-grants.json`, next to the settings file), by plugin
//! directory: its id, the permissions you answered for and the answer. An approval holds while the
//! plugin asks for no more than that (it may ask for less); asking for more asks you again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::discovery::{DiscoveredPlugin, PluginSource};

/// Whether a plugin may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// Bundled with Space.
    Trusted,
    Approved,
    Denied,
    /// Not answered yet, or it asks for more than you approved.
    Pending,
}

impl Approval {
    pub fn allows_start(self) -> bool {
        matches!(self, Approval::Trusted | Approval::Approved)
    }

    /// Why the plugin isn't started, when it isn't.
    pub fn refusal(self) -> Option<&'static str> {
        match self {
            Approval::Pending => Some("Waiting for your approval in Settings → Plugins."),
            Approval::Denied => Some("You didn't approve it."),
            Approval::Trusted | Approval::Approved => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Grant {
    id: String,
    permissions: Vec<String>,
    approved: bool,
}

/// The approvals, saved at `path` on every change (`None`: kept in memory only).
#[derive(Debug, Default)]
pub struct Grants {
    path: Option<PathBuf>,
    /// By plugin directory.
    grants: Mutex<BTreeMap<String, Grant>>,
}

impl Grants {
    pub fn load(path: Option<PathBuf>) -> Self {
        let grants = path.as_deref().map(read).unwrap_or_default();
        Self { path, grants: Mutex::new(grants) }
    }

    pub fn approval(&self, plugin: &DiscoveredPlugin) -> Approval {
        if plugin.source == PluginSource::Bundled {
            return Approval::Trusted;
        }
        let grants = self.grants.lock().unwrap();
        match grants.get(&key(plugin)) {
            // Same folder, same id: a plugin that changes its id is asked again.
            Some(g) if g.id == plugin.id() && plugin.manifest.permissions().iter().all(|p| g.permissions.contains(p)) => {
                if g.approved { Approval::Approved } else { Approval::Denied }
            }
            _ => Approval::Pending,
        }
    }

    /// Answer for the plugin's current permissions.
    pub fn answer(&self, plugin: &DiscoveredPlugin, approved: bool) -> Result<()> {
        let mut grants = self.grants.lock().unwrap();
        let grant = Grant { id: plugin.id().to_string(), permissions: plugin.manifest.permissions(), approved };
        grants.insert(key(plugin), grant);
        if let Some(path) = &self.path {
            write(path, &grants)?;
        }
        Ok(())
    }
}

fn key(plugin: &DiscoveredPlugin) -> String {
    plugin.dir.display().to_string()
}

fn read(path: &Path) -> BTreeMap<String, Grant> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!("{} is not valid ({e}); every plugin that needs approval asks again", path.display());
            BTreeMap::new()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => {
            tracing::warn!("reading {} failed ({e})", path.display());
            BTreeMap::new()
        }
    }
}

fn write(path: &Path, grants: &BTreeMap<String, Grant>) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut json = serde_json::to_string_pretty(grants)?;
    json.push('\n');
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// A permission from [`space_protocol::Manifest::permissions`] in plain words.
pub fn describe(permission: &str) -> String {
    use space_protocol::service::{AGENT_COMPLETE, HOST_NOTIFY, HOST_OPEN_URL, SESSIONS_READ};
    use space_protocol::{CATALOG_PERMISSION_MODES, CATALOG_PREFIX, HOOK_TOOLS_PRE_EXECUTE, USE_READ_TRANSCRIPT, manifest};
    let (kind, what) = permission.split_once(':').unwrap_or((permission, ""));
    let plain = match (kind, what) {
        ("required", _) => "Can't be turned off, and tool calls wait while it isn't running",
        ("provides", space_protocol::CAP_AGENT) => "Runs as a coding agent",
        ("provides", space_protocol::CAP_SANDBOX) => "Wraps every command the agent runs",
        ("provides", space_protocol::CAP_CONTEXT) => "Adds hidden context to every message you send",
        ("provides", space_protocol::CAP_SETTINGS) => "Has its own settings",
        ("provides", space_protocol::CAP_SESSION_STORE) => "Keeps your threads",
        ("provides", space_protocol::CAP_SESSION_TITLE) => "Names your threads",
        ("provides", space_protocol::CAP_TRAJECTORY) => "Shows each thread's trajectory",
        ("provides", c) if c.strip_prefix(CATALOG_PREFIX) == Some(CATALOG_PERMISSION_MODES) => "Offers the permission modes",
        ("provides", c) if c.starts_with(CATALOG_PREFIX) => return format!("Publishes the “{}” list", &c[CATALOG_PREFIX.len()..]),
        ("hook", HOOK_TOOLS_PRE_EXECUTE) => "Checks every tool call before it runs, and can block it",
        ("observes", manifest::EVENT_SESSION_CREATED) => "Is told when a thread starts",
        ("observes", manifest::EVENT_TURN_STARTED) => "Is told when the agent starts working",
        ("observes", manifest::EVENT_TURN_ENDED) => "Is told when the agent finishes",
        ("observes", manifest::EVENT_TOOL_HELD) => "Is told when a tool call waits for a guard",
        ("observes", manifest::EVENT_PERMISSION_REQUESTED) => "Is told when a tool call needs your approval",
        ("observes", manifest::EVENT_TOOL_RESULT) => "Is told which tools ran and how they ended",
        ("observes", manifest::EVENT_THREAD_TITLED) => "Is told when a thread gets its title",
        ("observes", manifest::EVENT_THREAD_DELETED) => "Is told when a thread is deleted",
        ("observes", manifest::EVENT_TRACE) => "Is told about every model request and tool call",
        ("uses", space_protocol::USE_READ_TRACE) => "Can read everything sent to and from the model, including file contents",
        ("uses", USE_READ_TRANSCRIPT) => "Reads what you and the agent write",
        ("uses", HOST_NOTIFY) => "Shows notifications",
        ("uses", HOST_OPEN_URL) => "Opens web links in your browser",
        ("uses", AGENT_COMPLETE) => "Asks the agent's model short questions",
        ("uses", SESSIONS_READ) => "Reads your saved threads",
        ("env", name) => return format!("Reads the environment variable {name}"),
        _ => return permission.to_string(),
    };
    plain.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use space_protocol::Manifest;

    fn plugin(dir: &Path, source: PluginSource, extra: &str) -> DiscoveredPlugin {
        let text = format!("id='p'\nname='P'\nversion='1'\nprovides=['context']\n{extra}\n[run]\ncommand='x'\n");
        DiscoveredPlugin { manifest: Manifest::parse(&text, Path::new("m.toml")).unwrap(), dir: dir.to_path_buf(), source }
    }

    #[test]
    fn approvals_hold_until_a_plugin_asks_for_more() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/plugin-grants.json");
        let dir = tmp.path().join("p");
        let grants = Grants::load(Some(path.clone()));
        assert_eq!(grants.approval(&plugin(&dir, PluginSource::Bundled, "")), Approval::Trusted);
        let user = plugin(&dir, PluginSource::User, "uses=['host/notify']");
        assert_eq!(grants.approval(&user), Approval::Pending);
        grants.answer(&user, true).unwrap();
        assert_eq!(grants.approval(&user), Approval::Approved);

        // Saved: a new load knows. Asking for less keeps the approval; for more asks again.
        let grants = Grants::load(Some(path.clone()));
        assert_eq!(grants.approval(&user), Approval::Approved);
        assert_eq!(grants.approval(&plugin(&dir, PluginSource::Project, "")), Approval::Approved);
        let grown = plugin(&dir, PluginSource::User, "uses=['host/notify', 'read:transcript']");
        assert_eq!(grants.approval(&grown), Approval::Pending);
        // The same plugin somewhere else is a different plugin.
        assert_eq!(grants.approval(&plugin(&tmp.path().join("q"), PluginSource::User, "")), Approval::Pending);
        // The same folder under another id asks again.
        let mut renamed = user.clone();
        renamed.manifest.id = "other".into();
        assert_eq!(grants.approval(&renamed), Approval::Pending);

        grants.answer(&grown, false).unwrap();
        assert_eq!(grants.approval(&grown), Approval::Denied);
        assert_eq!(Grants::load(Some(path)).approval(&user), Approval::Denied);
    }

    #[test]
    fn plain_words() {
        assert_eq!(describe("uses:read:transcript"), "Reads what you and the agent write");
        assert_eq!(describe("hook:tools/pre-execute"), "Checks every tool call before it runs, and can block it");
        assert_eq!(describe("provides:catalog:snippets"), "Publishes the “snippets” list");
        assert_eq!(describe("env:SSH_AUTH_SOCK"), "Reads the environment variable SSH_AUTH_SOCK");
        assert_eq!(describe("required"), "Can't be turned off, and tool calls wait while it isn't running");
    }
}
