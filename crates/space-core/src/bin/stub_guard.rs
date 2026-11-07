//! `space-stub-guard`: a scripted service plugin for tests and dev scenarios.
//!
//! Its behaviour comes from the JSON in `SPACE_STUB_GUARD` (every field optional):
//!
//! ```json
//! { "decision": "ask", "reason": "mode={mode} history={history}",
//!   "sandbox": ["/usr/bin/env"], "context": "Mode: {mode}",
//!   "catalog": [{ "id": "a", "name": "A", "default": true }],
//!   "secret": "api_key", "needsSecret": ["b"] }
//! ```
//!
//! * `decision` — the `tools/pre-execute` answer: `allow`, `deny`, `ask`, `next` (default), or
//!   `crash` (exit at once, so the host sees a failing listener).
//! * `reason` — `{mode}`, `{tool}` and `{history}` (the number of earlier items) are filled in.
//! * `sandbox` — `sandbox/confine` puts these words in front of the argv.
//! * `context` — one hidden context section with this text.
//! * `catalog` — the entries of `permission-modes`; ids listed in `needsSecret` are not ready
//!   until the secret setting named by `secret` is set.

use std::sync::{Mutex, OnceLock};

use serde::Deserialize;
use space_plugin::protocol::service::*;
use space_plugin::{Host, Plugin, RpcError};

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    decision: Option<String>,
    reason: Option<String>,
    sandbox: Option<Vec<String>>,
    context: Option<String>,
    #[serde(default)]
    catalog: Vec<CatalogEntry>,
    secret: Option<String>,
    #[serde(default)]
    needs_secret: Vec<String>,
}

struct Guard {
    config: Config,
    settings: Mutex<SettingValues>,
    host: OnceLock<Host>,
}

impl Guard {
    fn fill(&self, text: &str, mode: &str, tool: &str, history: usize) -> String {
        text.replace("{mode}", mode).replace("{tool}", tool).replace("{history}", &history.to_string())
    }
}

impl Plugin for Guard {
    async fn initialize(&self, init: Initialize, host: Host) -> Result<Initialized, RpcError> {
        eprintln!("space-stub-guard: initialized with {} setting(s)", init.settings.len());
        *self.settings.lock().unwrap() = init.settings;
        let _ = self.host.set(host);
        Ok(Initialized { name: Some("stub guard".into()) })
    }

    fn settings_schema(&self) -> SettingsSchema {
        let fields = self.config.secret.iter().map(|key| SettingField {
            key: key.clone(),
            label: "API key".into(),
            description: "Test secret".into(),
            kind: SettingKind::Secret,
            placeholder: None,
        });
        SettingsSchema { fields: fields.collect() }
    }

    fn settings_changed(&self, values: SettingValues) {
        eprintln!("space-stub-guard: settings changed ({} set)", values.len());
        *self.settings.lock().unwrap() = values;
        if let Some(host) = self.host.get() {
            host.catalog_changed(space_plugin::protocol::CATALOG_PERMISSION_MODES);
        }
    }

    fn catalog(&self, _name: &str) -> Result<Catalog, RpcError> {
        let has_secret = self.config.secret.as_ref().is_some_and(|k| self.settings.lock().unwrap().contains_key(k));
        let entries = self
            .config
            .catalog
            .iter()
            .cloned()
            .map(|mut e| {
                if self.config.needs_secret.contains(&e.id) && !has_secret {
                    e.ready = false;
                    e.blocker = Some(Blocker {
                        text: "Set up the key to use this mode".into(),
                        action_label: Some("Open Settings".into()),
                        action: Some("open-settings:guard".into()),
                    });
                }
                e
            })
            .collect();
        Ok(Catalog { entries })
    }

    async fn context(&self, request: ContextRequest) -> Result<ContextSections, RpcError> {
        let sections = self.config.context.iter().map(|t| ContextSection {
            id: "stub_guard".into(),
            text: self.fill(t, &request.mode, "", 0),
        });
        Ok(ContextSections { sections: sections.collect() })
    }

    async fn confine(&self, request: ConfineRequest) -> Result<Confined, RpcError> {
        let mut argv = self.config.sandbox.clone().unwrap_or_default();
        argv.extend(request.argv);
        Ok(Confined { argv })
    }

    async fn pre_execute(&self, call: HookCall) -> Result<HookDecision, RpcError> {
        let reason = self.config.reason.as_deref().unwrap_or("stub guard");
        let reason = self.fill(reason, &call.mode, &call.call.tool, call.history.len());
        eprintln!("space-stub-guard: {} {} in {}", call.call.tool, call.call.args, call.mode);
        Ok(match self.config.decision.as_deref().unwrap_or("next") {
            "allow" => HookDecision::allow(),
            "deny" => HookDecision::deny(reason),
            "ask" => HookDecision::ask(reason),
            "crash" => std::process::exit(7),
            _ => HookDecision::next(),
        })
    }
}

fn main() {
    let config = match std::env::var("SPACE_STUB_GUARD") {
        Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
            eprintln!("space-stub-guard: bad SPACE_STUB_GUARD: {e}");
            Config::default()
        }),
        Err(_) => Config::default(),
    };
    eprintln!("space-stub-guard: started");
    space_plugin::run(Guard { config, settings: Mutex::new(SettingValues::new()), host: OnceLock::new() });
}
