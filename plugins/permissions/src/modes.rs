//! The three permission modes: their catalog entries, the hidden context line each adds to a
//! prompt, and the Read Only rule for file tools.

use space_plugin::protocol::service::{Blocker, BlockerAction, CatalogEntry, ContextSection, HookDecision, ToolCallFacts};

use crate::health::{BILLING_URL, Health, State, unix_ms};

pub const READ_ONLY: &str = "read-only";
pub const WORKSPACE_WRITE: &str = "workspace-write";
pub const FULL_ACCESS: &str = "full-access";

/// The mode the Settings page's field is named after.
pub const API_KEY: &str = "openai_api_key";

/// The name of a mode id, for messages.
pub fn mode_name(id: &str) -> &'static str {
    match id {
        READ_ONLY => "Read Only",
        FULL_ACCESS => "Full access",
        _ => "Workspace Write",
    }
}

/// What a blocker offers in each state: a way to fix it, and "Retry now" where a retry can help.
fn actions(health: &Health) -> Vec<BlockerAction> {
    let settings = |label: &str| BlockerAction::new(label, "open-settings:permissions");
    let retry = BlockerAction::new("Retry now", "retry:permissions");
    match health.state {
        State::Healthy | State::Checking => Vec::new(),
        State::NotSet => vec![settings("Open Settings")],
        State::KeyRejected => vec![settings("Update key"), retry],
        State::NoAccess => vec![settings("Open Settings"), retry],
        State::NoCredit => vec![BlockerAction::new("Open billing", format!("open-url:{BILLING_URL}")), retry],
        State::Unreachable | State::Incompatible => vec![retry],
    }
}

/// The modes, in menu order. Workspace Write and Full access can be used only while the
/// decision model can (`health`); until then they carry a blocker saying why.
pub fn catalog(health: &Health) -> Vec<CatalogEntry> {
    let needs_model = |mut e: CatalogEntry| {
        if let Some(text) = health.problem(&e.name) {
            e.ready = false;
            e.blocker = Some(Blocker {
                text,
                actions: actions(health),
                last_attempt_ms: health.last_attempt.map(unix_ms),
                next_attempt_ms: health.next_attempt.map(unix_ms),
            });
        }
        e
    };
    vec![
        CatalogEntry {
            id: READ_ONLY.into(),
            name: "Read Only".into(),
            icon: Some("shield-check".into()),
            description: "Commands run in the macOS sandbox: nothing outside the temporary folder can be written. \
                          File edits are off."
                .into(),
            ready: true,
            ..Default::default()
        },
        needs_model(CatalogEntry {
            id: WORKSPACE_WRITE.into(),
            name: "Workspace Write".into(),
            icon: Some("shield-half".into()),
            description: "Changes inside the project folder, git included. A decision model checks every operation \
                          first."
                .into(),
            default: true,
            ready: true,
            ..Default::default()
        }),
        needs_model(CatalogEntry {
            id: FULL_ACCESS.into(),
            name: "Full access".into(),
            icon: Some("shield-alert".into()),
            description: "Changes anywhere on this Mac. A decision model still checks every operation first.".into(),
            confirm: Some(
                "Full access lets the agent change files anywhere on this Mac. A decision model still checks every \
                 operation first."
                    .into(),
            ),
            ready: true,
            ..Default::default()
        }),
    ]
}

/// The one neutral line the agent gets about its mode (hidden; never in the transcript).
pub fn context(mode: &str, project_root: &str, tmpdir: &str) -> Vec<ContextSection> {
    let text = match mode {
        READ_ONLY => format!(
            "Permission mode: Read Only. Commands run in a sandbox that blocks file writes outside {tmpdir}, and the \
             edit and write tools are turned off. Try what you need; a blocked action fails with a message saying why."
        ),
        FULL_ACCESS => "Permission mode: Full access. You may change files anywhere; every operation is checked by a \
                        guardrail first, and a blocked one fails with the reason."
            .to_string(),
        _ => format!(
            "Permission mode: Workspace Write. You may change anything inside {project_root}; every operation is \
             checked by a guardrail first, and a blocked one fails with the reason."
        ),
    };
    vec![ContextSection { id: "permissions".into(), text }]
}

/// A file-changing tool (edit, write and the like).
pub fn is_file_edit(call: &ToolCallFacts) -> bool {
    call.kind == "edit" || matches!(call.tool.as_str(), "edit" | "write" | "multiedit" | "apply_patch")
}

/// Read Only: file tools are denied; everything else is left to the sandbox.
pub fn read_only_check(call: &ToolCallFacts) -> HookDecision {
    if is_file_edit(call) {
        HookDecision::deny(
            "Read Only mode: file edits are turned off. Describe the change instead, or the user can switch the \
             thread to Workspace Write.",
        )
    } else {
        HookDecision::next()
    }
}

#[cfg(test)]
mod tests {
    use space_plugin::protocol::service::HookVerdict;

    use super::*;

    #[test]
    fn catalog_readiness_follows_health() {
        use std::time::{Duration, UNIX_EPOCH};

        use crate::health::Failure;

        let now = UNIX_EPOCH + Duration::from_secs(1000);
        let without = catalog(&Health::new(false, false, now));
        assert_eq!(without.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), [READ_ONLY, WORKSPACE_WRITE, FULL_ACCESS]);
        assert!(without[0].ready && !without[1].ready && !without[2].ready);
        let b = without[1].blocker.as_ref().unwrap();
        assert_eq!(b.actions[0].action, "open-settings:permissions");
        assert!(b.text == "Workspace Write needs an OpenAI API key.", "{}", b.text);
        assert!(without[1].default && without[2].confirm.is_some());

        let mut h = Health::new(true, false, now);
        h.failure(&Failure::new(State::NoCredit, "quota"), now, Duration::from_secs(300));
        let entries = catalog(&h);
        let b = entries[2].blocker.as_ref().unwrap();
        assert!(b.text.contains("out of credit"));
        let actions: Vec<&str> = b.actions.iter().map(|a| a.action.as_str()).collect();
        assert_eq!(actions, [format!("open-url:{BILLING_URL}").as_str(), "retry:permissions"]);
        assert_eq!((b.last_attempt_ms, b.next_attempt_ms), (Some(1_000_000), Some(1_300_000)));

        h.success(now);
        assert!(catalog(&h).iter().all(|e| e.ready && e.blocker.is_none()));
    }

    #[test]
    fn read_only_denies_file_tools_only() {
        let call = |tool: &str, kind: &str| ToolCallFacts { tool: tool.into(), kind: kind.into(), ..Default::default() };
        assert_eq!(read_only_check(&call("write", "edit")).decision, HookVerdict::Deny);
        assert_eq!(read_only_check(&call("edit", "other")).decision, HookVerdict::Deny);
        assert_eq!(read_only_check(&call("bash", "execute")).decision, HookVerdict::Next);
        assert_eq!(read_only_check(&call("read", "read")).decision, HookVerdict::Next);
        let text = &context(READ_ONLY, "/p", "/t")[0].text;
        assert!(text.contains("/t") && text.starts_with("Permission mode: Read Only"));
        assert!(context(WORKSPACE_WRITE, "/p", "/t")[0].text.contains("/p"));
    }
}
