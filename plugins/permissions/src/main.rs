//! `space-permissions`: the permissions service plugin (see README.md).
//!
//! * Read Only: commands are confined by the macOS sandbox; file-edit tools are denied.
//! * Workspace Write and Full access: no sandbox; every tool call is reviewed first by the
//!   decision model. While it can't be used (no key, key rejected, offline, …) calls are
//!   answered `unavailable` (the host holds them) and the modes carry a blocker; a probe
//!   retries every 5 minutes, or at once on "Retry now" or a new key, and on success the
//!   plugin sends `hooks/ready` so held calls go on.

mod health;
mod luna;
mod modes;
mod rules;
#[cfg(test)]
mod eval;
mod sandbox;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use space_plugin::protocol::CATALOG_PERMISSION_MODES;
use space_plugin::protocol::service::*;
use space_plugin::{Host, Plugin, RpcError};
use tokio::sync::Notify;

use crate::health::{Failure, Health, State};
use crate::luna::ReviewError;
use crate::modes::{API_KEY, READ_ONLY, mode_name};

struct Permissions {
    inner: Arc<Inner>,
}

struct Inner {
    /// The OpenAI API key (Settings ▸ Permissions, kept in the Keychain by the host).
    key: Mutex<Option<String>>,
    /// Bumped whenever the key changes: results for an older key are ignored.
    generation: AtomicU64,
    /// The value `settings/validate` just accepted (so it isn't probed twice).
    validated: Mutex<Option<String>>,
    health: Mutex<Health>,
    /// Wakes the probe loop (a key change, "Retry now", a new failure).
    wake: Notify,
    host: OnceLock<Host>,
    tmpdir: String,
    luna: luna::Luna,
}

impl Inner {
    fn now() -> SystemTime {
        SystemTime::now()
    }

    /// Change the health; tell the host what changed (the blockers; held calls on recovery).
    fn update(&self, f: impl FnOnce(&mut Health)) {
        let (before, after) = {
            let mut h = self.health.lock().unwrap();
            let before = h.clone();
            f(&mut h);
            (before, h.clone())
        };
        if before == after {
            return;
        }
        if before.state != after.state || before.detail != after.detail {
            eprintln!(
                "decision model: {:?}{}",
                after.state,
                after.detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default()
            );
        }
        self.wake.notify_one();
        if let Some(host) = self.host.get() {
            host.catalog_changed(CATALOG_PERMISSION_MODES);
            // Held calls are checked again: on a recovery they go on; otherwise their reason
            // is brought up to date (the answer is instant while the model can't be used).
            if before.state != after.state || before.detail != after.detail {
                host.hooks_ready();
            }
        }
    }

    fn fail(&self, generation: u64, f: &Failure) {
        if self.generation.load(Ordering::SeqCst) == generation {
            self.update(|h| h.failure(f, Self::now(), health::retry_interval()));
        }
    }

    fn succeed(&self, generation: u64) {
        if self.generation.load(Ordering::SeqCst) == generation && !self.health.lock().unwrap().ready() {
            self.update(|h| {
                h.success(Self::now());
            });
        }
    }

    /// Probe while the model can't be used: when due, or when woken.
    async fn probe_loop(self: Arc<Self>) {
        loop {
            let due = {
                let h = self.health.lock().unwrap();
                if h.probe_due(Self::now()) { Some(None) } else { h.next_attempt.filter(|_| !h.ready()).map(Some) }
            };
            match due {
                // Due now.
                Some(None) => {}
                Some(Some(at)) => {
                    let wait = at.duration_since(Self::now()).unwrap_or_default();
                    tokio::select! {
                        () = tokio::time::sleep(wait) => {}
                        () = self.wake.notified() => {}
                    }
                    continue;
                }
                None => {
                    self.wake.notified().await;
                    continue;
                }
            }
            let (key, generation) = (self.key.lock().unwrap().clone(), self.generation.load(Ordering::SeqCst));
            let Some(key) = key else {
                self.wake.notified().await;
                continue;
            };
            match self.luna.probe(&key).await {
                Ok(()) => self.succeed(generation),
                Err(f) => self.fail(generation, &f),
            }
        }
    }

    /// Workspace Write and Full access: the decision model's verdict, or `unavailable` (held)
    /// while it can't be used.
    async fn review(&self, call: &HookCall) -> HookDecision {
        let mode = mode_name(&call.mode);
        let tool = format!("decision mode={} tool={} call={}", call.mode, call.call.tool, call.call.id);
        // Plainly fine calls need no model (and go on while it can't be used).
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
        let temp = [std::path::PathBuf::from(&self.tmpdir), "/tmp".into(), "/private/tmp".into()];
        let facts = match rules::check(&call.call, &rules::Context { project_root: &call.project_root, home: &home, temp: &temp }) {
            rules::Ruling::Allow(why) => {
                eprintln!("{tool} verdict=allow by=rules ({why})");
                return HookDecision::allow();
            }
            rules::Ruling::Model(facts) => facts.describe(),
        };
        if let Some(problem) = self.health.lock().unwrap().problem(mode) {
            return HookDecision::unavailable(problem);
        }
        let (key, generation) = (self.key.lock().unwrap().clone(), self.generation.load(Ordering::SeqCst));
        let Some(key) = key else { return HookDecision::unavailable(format!("{mode} needs an OpenAI API key.")) };
        match self.luna.review(&key, call, facts.as_deref()).await {
            Ok(review) => {
                self.succeed(generation);
                eprintln!("{tool} by=model {}", review.log);
                review.decision
            }
            Err(ReviewError::TooLarge(what)) => {
                eprintln!("{tool} verdict=deny too large ({what})");
                HookDecision::deny(format!(
                    "Blocked: this call is too large for the guardrail to review ({what}). Split it into smaller steps."
                ))
            }
            Err(ReviewError::Failed(f)) => {
                eprintln!("{tool} verdict=held state={:?} detail=\"{}\"", f.state, f.detail);
                self.fail(generation, &f);
                let problem = self.health.lock().unwrap().problem(mode);
                HookDecision::unavailable(problem.unwrap_or_else(|| format!("The guardrail couldn't check this call ({}).", f.detail)))
            }
        }
    }

    fn set_key(&self, key: Option<String>, unreadable: bool) {
        let key = key.filter(|k| !k.trim().is_empty());
        if *self.key.lock().unwrap() == key && self.health.lock().unwrap().unreadable == unreadable {
            return;
        }
        *self.key.lock().unwrap() = key.clone();
        self.generation.fetch_add(1, Ordering::SeqCst);
        let just_validated = key.is_some() && *self.validated.lock().unwrap() == key;
        eprintln!("decision model key {}", if key.is_some() { "set" } else if unreadable { "unreadable" } else { "removed" });
        self.update(|h| {
            h.key_changed(key.is_some(), unreadable, Self::now());
            if just_validated {
                h.validated(Self::now());
            }
        });
    }
}

impl Permissions {
    fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                key: Mutex::new(None),
                generation: AtomicU64::new(0),
                validated: Mutex::new(None),
                health: Mutex::new(Health::new(false, false, SystemTime::now())),
                wake: Notify::new(),
                host: OnceLock::new(),
                tmpdir: sandbox::tmpdir().display().to_string(),
                luna: luna::Luna::new(),
            }),
        }
    }
}

impl Plugin for Permissions {
    async fn initialize(&self, init: Initialize, host: Host) -> Result<Initialized, RpcError> {
        let _ = self.inner.host.set(host);
        let unreadable = init.unreadable.iter().any(|k| k == API_KEY);
        self.inner.set_key(init.settings.get(API_KEY).cloned(), unreadable);
        tokio::spawn(self.inner.clone().probe_loop());
        eprintln!("ready; sandbox tmpdir {}", self.inner.tmpdir);
        Ok(Initialized { name: Some("Permissions".into()), ..Default::default() })
    }

    fn settings_schema(&self) -> SettingsSchema {
        SettingsSchema {
            fields: vec![SettingField {
                key: API_KEY.into(),
                label: "OpenAI API key".into(),
                description: "For the decision model (gpt-6-luna) that checks every operation in Workspace Write and \
                              Full access. Checked with OpenAI before it's saved."
                    .into(),
                kind: SettingKind::Secret,
                placeholder: Some("sk-…".into()),
                default: None,
            }],
        }
    }

    async fn validate_setting(&self, check: SettingCheck) -> Result<SettingVerdict, RpcError> {
        if check.key != API_KEY {
            return Ok(SettingVerdict::ok());
        }
        Ok(match self.inner.luna.probe(&check.value).await {
            Ok(()) => {
                *self.inner.validated.lock().unwrap() = Some(check.value);
                SettingVerdict::ok()
            }
            Err(f) => {
                eprintln!("key check failed: {:?} ({})", f.state, f.detail);
                match f.state {
                    State::KeyRejected => SettingVerdict::invalid("OpenAI rejected this key."),
                    State::NoAccess => SettingVerdict::invalid("This key's account doesn't have the Decisions API yet."),
                    State::NoCredit => SettingVerdict::invalid("OpenAI says this key's account is out of credit."),
                    State::Incompatible => SettingVerdict::invalid(format!(
                        "OpenAI's decision API didn't accept the check ({}); Space may need an update.",
                        f.detail
                    )),
                    _ => SettingVerdict::unverifiable(format!("Couldn't reach OpenAI to check it ({}).", f.detail)),
                }
            }
        })
    }

    fn settings_changed(&self, changed: SettingsChanged) {
        let unreadable = changed.unreadable.iter().any(|k| k == API_KEY);
        self.inner.set_key(changed.values.get(API_KEY).cloned(), unreadable);
    }

    fn retry(&self) {
        eprintln!("retry requested");
        self.inner.update(|h| h.retry_now(Inner::now()));
        self.inner.wake.notify_one();
    }

    fn catalog(&self, name: &str) -> Result<Catalog, RpcError> {
        if name != CATALOG_PERMISSION_MODES {
            return Err(RpcError::invalid_params(format!("no catalog {name}")));
        }
        Ok(Catalog { entries: modes::catalog(&self.inner.health.lock().unwrap()) })
    }

    async fn context(&self, request: ContextRequest) -> Result<ContextSections, RpcError> {
        Ok(ContextSections { sections: modes::context(&request.mode, &request.project_root, &self.inner.tmpdir) })
    }

    async fn confine(&self, request: ConfineRequest) -> Result<Confined, RpcError> {
        let argv = if request.mode == READ_ONLY {
            sandbox::confine_read_only(request.argv, &self.inner.tmpdir)
        } else {
            request.argv
        };
        Ok(Confined { argv })
    }

    async fn pre_execute(&self, call: HookCall) -> Result<HookDecision, RpcError> {
        if call.mode == READ_ONLY {
            let decision = modes::read_only_check(&call.call);
            if decision.decision == HookVerdict::Deny {
                eprintln!("decision mode={} tool={} verdict=deny (file edits are off)", call.mode, call.call.tool);
            }
            return Ok(decision);
        }
        Ok(self.inner.review(&call).await)
    }
}

fn main() {
    space_plugin::run(Permissions::new());
}
