//! The three permission modes: their catalog entries, the hidden context line each adds to a
//! prompt, and the Read Only rule for file tools.

use space_plugin::protocol::service::{Blocker, CatalogEntry, ContextSection, HookDecision, ToolCallFacts};

pub const READ_ONLY: &str = "read-only";
pub const WORKSPACE_WRITE: &str = "workspace-write";
pub const FULL_ACCESS: &str = "full-access";

/// The mode the Settings page's field is named after.
pub const API_KEY: &str = "openai_api_key";

/// The modes, in menu order. Workspace Write and Full access need the decision model's key.
pub fn catalog(has_key: bool) -> Vec<CatalogEntry> {
    let needs_key = |mut e: CatalogEntry| {
        if !has_key {
            e.ready = false;
            e.blocker = Some(Blocker {
                text: format!("{} needs the decision model. Add an OpenAI API key to use it.", e.name),
                action_label: Some("Open Settings".into()),
                action: Some("open-settings:permissions".into()),
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
        needs_key(CatalogEntry {
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
        needs_key(CatalogEntry {
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
    fn catalog_readiness_follows_the_key() {
        let without = catalog(false);
        assert_eq!(without.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), [READ_ONLY, WORKSPACE_WRITE, FULL_ACCESS]);
        assert!(without[0].ready && !without[1].ready && !without[2].ready);
        assert_eq!(without[1].blocker.as_ref().unwrap().action.as_deref(), Some("open-settings:permissions"));
        assert!(without[1].default && without[2].confirm.is_some());
        assert!(catalog(true).iter().all(|e| e.ready && e.blocker.is_none()));
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
