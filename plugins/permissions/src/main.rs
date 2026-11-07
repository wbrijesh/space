//! `space-permissions`: the permissions service plugin (see README.md).
//!
//! * Read Only: commands are confined by the macOS sandbox; file-edit tools are denied.
//! * Workspace Write and Full access: no sandbox; every tool call is reviewed first.

mod luna;
mod modes;
mod sandbox;

use std::sync::{Mutex, OnceLock};

use space_plugin::protocol::CATALOG_PERMISSION_MODES;
use space_plugin::protocol::service::*;
use space_plugin::{Host, Plugin, RpcError};

use crate::modes::{API_KEY, READ_ONLY};

struct Permissions {
    /// The OpenAI API key (Settings ▸ Permissions, kept in the Keychain by the host).
    key: Mutex<Option<String>>,
    host: OnceLock<Host>,
    tmpdir: String,
    luna: luna::Luna,
}

impl Permissions {
    fn new() -> Self {
        Self {
            key: Mutex::new(None),
            host: OnceLock::new(),
            tmpdir: sandbox::tmpdir().display().to_string(),
            luna: luna::Luna::new(),
        }
    }

    fn set_key(&self, settings: &SettingValues) {
        *self.key.lock().unwrap() = settings.get(API_KEY).cloned().filter(|k| !k.trim().is_empty());
    }

    fn has_key(&self) -> bool {
        self.key.lock().unwrap().is_some()
    }

    /// Workspace Write and Full access: the decision model's verdict (logged, one line per call).
    async fn review(&self, call: &HookCall) -> HookDecision {
        let Some(key) = self.key.lock().unwrap().clone() else {
            return HookDecision::ask(
                "The decision model isn't set up (Settings ▸ Permissions), so Space can't check this call.",
            );
        };
        let review = self.luna.review(&key, call).await;
        eprintln!("decision mode={} tool={} call={} {}", call.mode, call.call.tool, call.call.id, review.log);
        review.decision
    }
}

impl Plugin for Permissions {
    async fn initialize(&self, init: Initialize, host: Host) -> Result<Initialized, RpcError> {
        self.set_key(&init.settings);
        let _ = self.host.set(host);
        eprintln!("ready; decision model key {}; sandbox tmpdir {}", if self.has_key() { "set" } else { "not set" }, self.tmpdir);
        Ok(Initialized { name: Some("Permissions".into()) })
    }

    fn settings_schema(&self) -> SettingsSchema {
        SettingsSchema {
            fields: vec![SettingField {
                key: API_KEY.into(),
                label: "OpenAI API key".into(),
                description: "For the decision model (gpt-6-luna) that checks every operation in Workspace Write and \
                              Full access."
                    .into(),
                kind: SettingKind::Secret,
                placeholder: Some("sk-…".into()),
            }],
        }
    }

    fn settings_changed(&self, values: SettingValues) {
        let had = self.has_key();
        self.set_key(&values);
        eprintln!("decision model key {}", if self.has_key() { "set" } else { "removed" });
        if had != self.has_key()
            && let Some(host) = self.host.get()
        {
            host.catalog_changed(CATALOG_PERMISSION_MODES);
        }
    }

    fn catalog(&self, name: &str) -> Result<Catalog, RpcError> {
        if name != CATALOG_PERMISSION_MODES {
            return Err(RpcError::invalid_params(format!("no catalog {name}")));
        }
        Ok(Catalog { entries: modes::catalog(self.has_key()) })
    }

    async fn context(&self, request: ContextRequest) -> Result<ContextSections, RpcError> {
        Ok(ContextSections { sections: modes::context(&request.mode, &request.project_root, &self.tmpdir) })
    }

    async fn confine(&self, request: ConfineRequest) -> Result<Confined, RpcError> {
        let argv = if request.mode == READ_ONLY {
            sandbox::confine_read_only(request.argv, &self.tmpdir)
        } else {
            request.argv
        };
        Ok(Confined { argv })
    }

    async fn pre_execute(&self, call: HookCall) -> Result<HookDecision, RpcError> {
        let decision = if call.mode == READ_ONLY { modes::read_only_check(&call.call) } else { self.review(&call).await };
        if call.mode == READ_ONLY && decision.decision == HookVerdict::Deny {
            eprintln!("decision mode={} tool={} verdict=deny (file edits are off)", call.mode, call.call.tool);
        }
        Ok(decision)
    }
}

fn main() {
    space_plugin::run(Permissions::new());
}
