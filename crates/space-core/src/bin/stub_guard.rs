//! `space-stub-guard`: a scripted service plugin for tests and dev scenarios.
//!
//! Its behaviour comes from the JSON in `SPACE_STUB_GUARD` (every field optional):
//!
//! ```json
//! { "decision": "ask", "reason": "mode={mode} history={history}",
//!   "sandbox": ["/usr/bin/env"], "context": "Mode: {mode}",
//!   "catalog": [{ "id": "a", "name": "A", "default": true }],
//!   "secret": "api_key", "needsSecret": ["b"],
//!   "readyFile": "/tmp/x", "invalid": ["bad"], "unverifiable": ["offline"] }
//! ```
//!
//! * `decision` — the `tools/pre-execute` answer: `allow`, `deny`, `ask`, `next` (default),
//!   `unavailable`, or `crash` (exit at once, so the host sees a failing listener).
//! * `readyFile` — answer `unavailable` until this file exists; once it appears, send
//!   `hooks/ready` (checked every 50 ms).
//! * `holdModes` — answer `unavailable` in these permission modes.
//! * `crashUntil` — exit at once on every tool call or `sandbox/confine` until this file exists.
//! * `reason` — `{mode}`, `{tool}`, `{history}` (the number of earlier items) and `{said}`
//!   (the user's messages in the history, joined by `|`) are filled in.
//! * `call` — call this host service once initialized (with `callParams`, default `{}`), and
//!   log the answer (`space-stub-guard: <method> -> ok|<error>`).
//! * `observeDelayMs` — take this long over each observed event. Every observed event is
//!   logged as `space-stub-guard: observed <event JSON>`.
//! * `printEnv` — log which of these environment variables are set
//!   (`space-stub-guard: env HOME=set CARGO=unset`).
//! * `sandbox` — `sandbox/confine` puts these words in front of the argv.
//! * `context` — one hidden context section with this text.
//! * `catalog` — the entries of `permission-modes`; ids listed in `needsSecret` are not ready
//!   until the secret setting named by `secret` is set.
//! * `invalid`, `unverifiable` — `settings/validate` answers so for these values (else ok).

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
    ready_file: Option<String>,
    #[serde(default)]
    hold_modes: Vec<String>,
    crash_until: Option<String>,
    #[serde(default)]
    invalid: Vec<String>,
    #[serde(default)]
    unverifiable: Vec<String>,
    call: Option<String>,
    call_params: Option<serde_json::Value>,
    observe_delay_ms: Option<u64>,
    #[serde(default)]
    print_env: Vec<String>,
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
        if let Some(path) = self.config.ready_file.clone() {
            let host = host.clone();
            tokio::spawn(async move {
                while !std::path::Path::new(&path).exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                eprintln!("space-stub-guard: ready");
                host.hooks_ready();
            });
        }
        if let Some(method) = self.config.call.clone() {
            let host = host.clone();
            let params = self.config.call_params.clone().unwrap_or_else(|| serde_json::json!({}));
            tokio::spawn(async move {
                let answer = match host.request(&method, params).await {
                    Ok(_) => "ok".to_string(),
                    Err(e) => e.to_string(),
                };
                eprintln!("space-stub-guard: {method} -> {answer}");
            });
        }
        let _ = self.host.set(host);
        Ok(Initialized { name: Some("stub guard".into()), ..Default::default() })
    }

    fn settings_schema(&self) -> SettingsSchema {
        let fields = self.config.secret.iter().map(|key| SettingField {
            key: key.clone(),
            label: "API key".into(),
            description: "Test secret".into(),
            kind: SettingKind::Secret,
            placeholder: None,
            default: None,
        });
        SettingsSchema { fields: fields.collect() }
    }

    async fn validate_setting(&self, check: SettingCheck) -> Result<SettingVerdict, RpcError> {
        Ok(if self.config.invalid.contains(&check.value) {
            SettingVerdict::invalid("That key was rejected.")
        } else if self.config.unverifiable.contains(&check.value) {
            SettingVerdict::unverifiable("Couldn't reach the service.")
        } else {
            SettingVerdict::ok()
        })
    }

    fn settings_changed(&self, changed: SettingsChanged) {
        eprintln!("space-stub-guard: settings changed ({} set)", changed.values.len());
        *self.settings.lock().unwrap() = changed.values;
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
                        actions: vec![BlockerAction::new("Open Settings", "open-settings:guard")],
                        ..Default::default()
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
        if let Some(path) = &self.config.crash_until
            && !std::path::Path::new(path).exists()
        {
            std::process::exit(7);
        }
        let mut argv = self.config.sandbox.clone().unwrap_or_default();
        argv.extend(request.argv);
        Ok(Confined { argv })
    }

    async fn observe(&self, event: Observed) {
        eprintln!("space-stub-guard: observed {}", serde_json::to_string(&event).unwrap_or_default());
        if let Some(ms) = self.config.observe_delay_ms {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }
    }

    async fn pre_execute(&self, call: HookCall) -> Result<HookDecision, RpcError> {
        let reason = self.config.reason.as_deref().unwrap_or("stub guard");
        let said: Vec<&str> = call
            .history
            .iter()
            .filter_map(|i| match i {
                HistoryItem::User { text } => Some(text.as_str()),
                HistoryItem::Tool { .. } => None,
            })
            .collect();
        let reason = self.fill(reason, &call.mode, &call.call.tool, call.history.len()).replace("{said}", &said.join("|"));
        eprintln!("space-stub-guard: {} {} in {}", call.call.tool, call.call.args, call.mode);
        if let Some(path) = &self.config.crash_until
            && !std::path::Path::new(path).exists()
        {
            std::process::exit(7);
        }
        let missing = |p: &Option<String>| p.as_ref().is_some_and(|p| !std::path::Path::new(p).exists());
        if missing(&self.config.ready_file) || self.config.hold_modes.contains(&call.mode) {
            return Ok(HookDecision::unavailable(reason));
        }
        Ok(match self.config.decision.as_deref().unwrap_or("next") {
            "allow" => HookDecision::allow(),
            "deny" => HookDecision::deny(reason),
            "ask" => HookDecision::ask(reason),
            "unavailable" => HookDecision::unavailable(reason),
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
    if !config.print_env.is_empty() {
        let vars: Vec<String> = config
            .print_env
            .iter()
            .map(|v| format!("{v}={}", if std::env::var_os(v).is_some() { "set" } else { "unset" }))
            .collect();
        eprintln!("space-stub-guard: env {}", vars.join(" "));
    }
    space_plugin::run(Guard { config, settings: Mutex::new(SettingValues::new()), host: OnceLock::new() });
}
