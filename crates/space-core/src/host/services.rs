//! Service plugins: starting and restarting them, their settings and catalogs, and the seams
//! the host calls (`context`, `sandbox`).

use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde::de::DeserializeOwned;
use space_plugin::rpc::{self, Peer, RpcError};
use space_protocol::service::{self as svc, SettingField, SettingKind, SettingValues};
use space_protocol::manifest::EVENT_TRACE;
use space_protocol::trace::{TRAJECTORY_CHANGED, TrajectoryChanged};
use space_protocol::{CAP_CONTEXT, CAP_SANDBOX, CAP_SETTINGS, CAP_TRAJECTORY, Confine, ContextSection, USE_READ_TRACE};

use super::observers::Observer;
use super::traces::TraceSink;
use super::{Host, HostEvent, HostInner, PluginStatus, SettingError, SettingState, check_protocol, exit_status, start_failure};
use crate::discovery::DiscoveredPlugin;
use crate::process::{self, PluginProcess};

/// `settings/validate` may reach the network.
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(20);
/// This many crashes within [`CRASH_WINDOW`] and a held call says the plugin keeps crashing.
const CRASH_LIMIT: usize = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(300);
/// Other service calls (confine, context, catalogs, settings).
const SERVICE_TIMEOUT: Duration = Duration::from_secs(5);
/// A service that exited is restarted on next use, at most this often.
const RESTART_BACKOFF: Duration = Duration::from_secs(3);

/// A running service plugin.
pub(super) struct Service {
    pub(super) plugin: DiscoveredPlugin,
    peer: Peer,
    _process: PluginProcess,
    dead: Arc<AtomicBool>,
    fields: Vec<SettingField>,
    /// The plugin's event queue, when it `observes` anything.
    pub(super) observer: Option<Observer>,
    /// Its trace queue, when it observes `trace` (with `read:trace`).
    pub(super) tracer: Option<TraceSink>,
}

impl Service {
    pub(super) fn alive(&self) -> bool {
        !self.dead.load(Ordering::SeqCst) && !self.peer.is_closed()
    }
}

impl Host {
    /// Start every enabled service plugin that isn't running. Failures are reported as
    /// [`HostEvent::PluginStatus`] and don't stop the others.
    pub async fn start_services(&self) {
        let inner = self.inner.clone();
        let _ = self
            .inner
            .rt
            .spawn(async move {
                // Not approved ones too, so they say why they don't start.
                let disabled = inner.disabled.lock().unwrap().clone();
                let plugins = inner.plugins.lock().unwrap().clone();
                let ids: Vec<String> =
                    plugins.iter().filter(|p| !p.manifest.is_agent() && !disabled.contains(p.id())).map(|p| p.id().to_string()).collect();
                futures::future::join_all(ids.iter().map(|id| inner.service(id))).await;
            })
            .await;
    }

    /// Stop and start a service plugin again.
    pub async fn restart_service(&self, plugin_id: &str) -> Result<()> {
        // Back in a moment: what needs it keeps running (calls to it wait or fail closed).
        self.inner.kill_service(plugin_id);
        self.inner.started_at.lock().unwrap().remove(plugin_id);
        let inner = self.inner.clone();
        let id = plugin_id.to_string();
        self.inner.rt.spawn(async move { inner.service(&id).await.map(|_| ()) }).await?.map_err(|e| anyhow!(e))
    }

    /// "Retry now" on a blocker of `plugin_id`: tell it (starting it first if it isn't
    /// running), and run held calls through the chain again.
    pub async fn retry_plugin(&self, plugin_id: &str) {
        let inner = self.inner.clone();
        let id = plugin_id.to_string();
        let _ = self
            .inner
            .rt
            .spawn(async move {
                inner.started_at.lock().unwrap().remove(&id);
                match inner.service(&id).await {
                    Ok(service) => service.peer.notify(svc::PLUGIN_RETRY, serde_json::json!({})),
                    Err(e) => tracing::warn!("retry `{id}`: {e}"),
                }
                inner.wake_held();
            })
            .await;
    }

    /// Text settings the app saved for `plugin_id` (call before [`Host::start_services`]).
    pub fn set_text_settings(&self, plugin_id: &str, values: SettingValues) {
        self.inner.text_settings.lock().unwrap().insert(plugin_id.to_string(), values);
    }

    /// The settings of a running service plugin, and which are set.
    pub fn plugin_settings(&self, plugin_id: &str) -> Vec<SettingState> {
        let service = self.inner.services.lock().unwrap().get(plugin_id).cloned();
        service.map(|s| self.inner.setting_states(&s)).unwrap_or_default()
    }

    /// Change one setting of a service plugin (`None` clears it). A new value is first checked
    /// by the plugin (`settings/validate`): an invalid one isn't stored, and one that couldn't
    /// be checked is stored only with `force`. Secrets go to the Keychain; a text value is kept
    /// for the plugin (the app persists it). The plugin is told every setting's new value.
    pub async fn set_setting(
        &self,
        plugin_id: &str,
        key: &str,
        value: Option<String>,
        force: bool,
    ) -> Result<(), SettingError> {
        let inner = self.inner.clone();
        let (plugin_id, key) = (plugin_id.to_string(), key.to_string());
        let failed = |e: anyhow::Error| SettingError::Failed(format!("{e:#}"));
        self.inner
            .rt
            .spawn(async move {
                let service = inner.service(&plugin_id).await.map_err(SettingError::Failed)?;
                let field = service
                    .fields
                    .iter()
                    .find(|f| f.key == key)
                    .ok_or_else(|| SettingError::Failed(format!("`{plugin_id}` has no setting `{key}`")))?;
                let value = value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
                if let Some(v) = &value {
                    let check = svc::SettingCheck { key: key.clone(), value: v.clone() };
                    match service.peer.call::<_, svc::SettingVerdict>(svc::SETTINGS_VALIDATE, &check, Some(VALIDATE_TIMEOUT)).await {
                        Ok(verdict) => match verdict.status {
                            svc::SettingStatus::Ok => {}
                            svc::SettingStatus::Invalid => {
                                return Err(SettingError::Invalid(verdict.message.unwrap_or_else(|| "Not accepted.".into())));
                            }
                            svc::SettingStatus::Unverifiable if !force => {
                                let message = verdict.message.unwrap_or_else(|| "It couldn't be checked right now.".into());
                                return Err(SettingError::Unverifiable(message));
                            }
                            svc::SettingStatus::Unverifiable => {}
                        },
                        // A plugin that doesn't validate accepts everything.
                        Err(e) if e.code == RpcError::METHOD_NOT_FOUND => {}
                        Err(e) if force => tracing::warn!("validating `{plugin_id}`.{key}: {e}"),
                        Err(e) => return Err(SettingError::Unverifiable(format!("It couldn't be checked ({}).", e.message))),
                    }
                }
                match field.kind {
                    SettingKind::Secret => {
                        let secrets = inner.options.secrets.clone();
                        let (id, k, v) = (plugin_id.clone(), key.clone(), value.clone());
                        tokio::task::spawn_blocking(move || secrets.set(&id, &k, v.as_deref()))
                            .await
                            .map_err(|e| failed(anyhow!(e)))?
                            .map_err(failed)?;
                    }
                    SettingKind::Text | SettingKind::Switch | SettingKind::Model => {
                        let mut text = inner.text_settings.lock().unwrap();
                        let map = text.entry(plugin_id.clone()).or_default();
                        match value {
                            Some(v) => map.insert(key.clone(), v),
                            None => map.remove(&key),
                        };
                    }
                }
                let (values, unreadable) = inner.setting_values(&service).await;
                service.peer.notify(svc::SETTINGS_CHANGED, svc::SettingsChanged { values, unreadable });
                inner.emit(HostEvent::PluginSettings {
                    plugin_id: plugin_id.clone(),
                    settings: inner.setting_states(&service),
                });
                Ok(())
            })
            .await
            .map_err(|e| failed(anyhow!(e)))?
    }
}

impl HostInner {
    /// Stop a service for good (turned off, Space quitting), after the plugins that need it.
    pub(super) fn stop_service(&self, plugin_id: &str) {
        self.stop_dependents(plugin_id);
        self.kill_service(plugin_id);
    }

    /// Stop a service's process and drop what it published, leaving what needs it alone.
    fn kill_service(&self, plugin_id: &str) {
        let Some(service) = self.services.lock().unwrap().remove(plugin_id) else { return };
        service._process.kill();
        let mut catalogs = self.catalogs.lock().unwrap();
        for name in service.plugin.manifest.catalogs() {
            if catalogs.remove(name).is_some() {
                self.emit(HostEvent::Catalog { name: name.to_string(), entries: Vec::new() });
            }
        }
    }

    /// Stop the running plugins that need what `plugin_id` provides (their dependents first).
    /// They start again once it runs again.
    pub(super) fn stop_dependents(&self, plugin_id: &str) {
        let Some(plugin) = self.plugin(plugin_id) else { return };
        for dep in self.dependents(&plugin).into_iter().filter(|d| self.is_running(d)) {
            tracing::info!("stopping `{}`: `{plugin_id}` stopped", dep.id());
            self.emit_status(dep.id(), PluginStatus::NotStarted(format!("Waiting for {}.", plugin.manifest.name)));
            // Stopped, not crashed: it may start again at once.
            self.started_at.lock().unwrap().remove(dep.id());
            if dep.manifest.is_agent() {
                if let Some(agent) = self.agents.lock().unwrap().get(dep.id()).and_then(Weak::upgrade) {
                    agent.stop();
                }
            } else {
                self.stop_service(dep.id());
            }
        }
    }

    /// Start what `plugin` requires (a provider of each), or say why it can't run.
    pub(super) async fn start_requirements(self: &Arc<Self>, plugin: &DiscoveredPlugin) -> Result<(), String> {
        if let Some(why) = self.unmet_requirement(plugin) {
            return Err(why);
        }
        for cap in &plugin.manifest.requires {
            let mut failure = String::new();
            for provider in self.providers(cap) {
                match Box::pin(self.service(provider.id())).await {
                    Ok(_) => {
                        failure.clear();
                        break;
                    }
                    Err(e) => failure = format!("{} isn't running: {e}", provider.manifest.name),
                }
            }
            if !failure.is_empty() {
                return Err(format!("Waiting for its {cap} provider ({failure})."));
            }
        }
        Ok(())
    }

    /// Start the services waiting for `plugin`, which runs now (the app restarts its agent).
    fn start_dependents(self: &Arc<Self>, plugin: &DiscoveredPlugin) {
        for dep in self.dependents(plugin) {
            if !dep.manifest.is_agent() && !self.is_running(&dep) {
                let host = self.clone();
                self.rt.spawn(async move { host.service(dep.id()).await });
            }
        }
    }

    /// A service that others need crashed: start it again after the backoff (unless it keeps
    /// crashing; then Restart or Retry does). What needs it keeps running meanwhile: held tool
    /// calls wait for it and sandboxed commands fail closed, so a turn isn't lost to a crash.
    fn provider_crashed(self: &Arc<Self>, plugin_id: &str) {
        let Some(plugin) = self.plugin(plugin_id) else { return };
        if !self.dependents(&plugin).iter().any(|d| self.is_running(d)) {
            return;
        }
        if !self.keeps_crashing(plugin_id) {
            let (host, id) = (self.clone(), plugin_id.to_string());
            self.rt.spawn(async move {
                tokio::time::sleep(RESTART_BACKOFF).await;
                let _ = host.service(&id).await;
            });
        }
    }

    /// The running service `plugin_id`, started (or restarted) first if needed, after what
    /// it requires.
    pub(super) async fn service(self: &Arc<Self>, plugin_id: &str) -> Result<Arc<Service>, String> {
        if let Some(s) = self.services.lock().unwrap().get(plugin_id).filter(|s| s.alive()) {
            return Ok(s.clone());
        }
        let lock = self.start_locks.lock().unwrap().entry(plugin_id.to_string()).or_default().clone();
        let _starting = lock.lock().await;
        // Started by whoever held the lock before us?
        if let Some(s) = self.services.lock().unwrap().get(plugin_id).filter(|s| s.alive()) {
            return Ok(s.clone());
        }
        if self.disabled.lock().unwrap().contains(plugin_id) {
            return Err(format!("`{plugin_id}` is turned off"));
        }
        let plugin = self
            .plugins
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.id() == plugin_id && !p.manifest.is_agent())
            .cloned()
            .ok_or_else(|| format!("no service plugin `{plugin_id}`"))?;
        if let Err(why) = self.start_requirements(&plugin).await {
            self.emit_status(plugin_id, PluginStatus::NotStarted(why.clone()));
            return Err(why);
        }
        {
            let mut started = self.started_at.lock().unwrap();
            if started.get(plugin_id).is_some_and(|t| t.elapsed() < RESTART_BACKOFF) {
                return Err(format!("`{plugin_id}` isn't running (it exited and was restarted moments ago)"));
            }
            started.insert(plugin_id.to_string(), Instant::now());
        }
        self.emit_status(plugin_id, PluginStatus::Starting);
        match self.start_service(plugin).await {
            Ok(service) => {
                self.emit_status(plugin_id, PluginStatus::Running);
                self.start_dependents(&service.plugin);
                Ok(service)
            }
            Err(e) => {
                self.emit_status(plugin_id, start_failure(&e));
                Err(format!("{e:#}"))
            }
        }
    }

    pub(super) async fn start_service(self: &Arc<Self>, plugin: DiscoveredPlugin) -> Result<Arc<Service>> {
        let plugin_id = plugin.id().to_string();
        let dead = Arc::new(AtomicBool::new(false));
        let launch = self.launch(&plugin_id);
        let spawned = {
            let weak = Arc::downgrade(self);
            let exit_weak = weak.clone();
            let (log_id, exit_id, exit_dead) = (plugin_id.clone(), plugin_id.clone(), dead.clone());
            process::spawn(
                &plugin,
                &self.options.project_dir,
                move |line| {
                    if let Some(host) = weak.upgrade() {
                        host.emit(HostEvent::PluginLog { plugin_id: log_id.clone(), line });
                    }
                },
                move |kind, tail| {
                    exit_dead.store(true, Ordering::SeqCst);
                    if let Some(host) = exit_weak.upgrade().filter(|h| h.is_current(&exit_id, launch)) {
                        let status = exit_status(kind, &tail);
                        let crashed = matches!(status, PluginStatus::Crashed(_));
                        if crashed {
                            host.crashed(&exit_id);
                        }
                        host.emit_exit(&exit_id, status);
                        if crashed {
                            host.provider_crashed(&exit_id);
                        }
                    }
                },
            )?
        };
        let on_request: rpc::RequestHandler = {
            let (weak, plugin) = (Arc::downgrade(self), plugin.clone());
            Arc::new(move |method: String, params| {
                let (weak, plugin) = (weak.clone(), plugin.clone());
                Box::pin(async move {
                    let host = weak.upgrade().ok_or_else(|| RpcError::unavailable("Space is quitting"))?;
                    host.host_service(&plugin, &method, params).await
                })
            })
        };
        let on_notification: rpc::NotificationHandler = {
            let (weak, plugin) = (Arc::downgrade(self), plugin.clone());
            Arc::new(move |method: String, params| {
                let Some(host) = weak.upgrade() else { return };
                // Only about what the manifest declares.
                match method.as_str() {
                    svc::CATALOG_CHANGED if plugin.manifest.catalogs().next().is_some() => {
                        let id = plugin.id().to_string();
                        host.rt.clone().spawn(async move { host.refresh_catalogs(&id).await });
                    }
                    svc::HOOKS_READY if !plugin.manifest.hooks.is_empty() => host.wake_held(),
                    TRAJECTORY_CHANGED if plugin.manifest.provides(CAP_TRAJECTORY) => {
                        match rpc::params::<TrajectoryChanged>(params) {
                            Ok(c) => host.emit(HostEvent::TrajectoryChanged { thread_id: c.thread_id, version: c.version }),
                            Err(e) => tracing::warn!("bad {TRAJECTORY_CHANGED} from `{}`: {e}", plugin.id()),
                        }
                    }
                    _ => {}
                }
            })
        };
        let (peer, _) = Peer::spawn(spawned.stdout, spawned.stdin, on_request, on_notification);
        let process = spawned.process;
        let logs = process.logs.clone();
        let fail = |what: &str, e: RpcError| {
            let tail = logs.tail(5).join("\n");
            if tail.is_empty() { anyhow!("{what} failed: {e}") } else { anyhow!("{what} failed: {e}\nplugin stderr:\n{tail}") }
        };
        let fields = if plugin.manifest.provides(CAP_SETTINGS) {
            let schema: svc::SettingsSchema = peer
                .call(svc::SETTINGS_SCHEMA, &serde_json::json!({}), Some(SERVICE_TIMEOUT))
                .await
                .map_err(|e| fail(svc::SETTINGS_SCHEMA, e))?;
            schema.fields
        } else {
            Vec::new()
        };
        let observer = (!plugin.manifest.observes.is_empty()).then(|| Observer::start(&self.rt, peer.clone(), plugin_id.clone()));
        let tracer = (plugin.manifest.observes(EVENT_TRACE) && plugin.manifest.uses(USE_READ_TRACE))
            .then(|| TraceSink::start(&self.rt, peer.clone(), plugin_id.clone()));
        let mut service = Service { plugin, peer, _process: process, dead, fields, observer, tracer };
        let (settings, unreadable) = self.setting_values(&service).await;
        let space_dir = self.options.space_dir.as_ref().map(|d| d.display().to_string());
        let init = svc::Initialize { protocol_version: svc::PROTOCOL_VERSION, settings, unreadable, space_dir };
        let initialized: svc::Initialized = service
            .peer
            .call(svc::INITIALIZE, &init, Some(SERVICE_TIMEOUT))
            .await
            .map_err(|e| fail(svc::INITIALIZE, e))?;
        check_protocol(initialized.protocol_version)?;
        service.fields.retain(|f| !f.key.is_empty());
        let service = Arc::new(service);
        self.services.lock().unwrap().insert(plugin_id.clone(), service.clone());
        self.refresh_catalogs(&plugin_id).await;
        if !service.fields.is_empty() {
            self.emit(HostEvent::PluginSettings { plugin_id, settings: self.setting_states(&service) });
        }
        // A (re)started listener may decide calls held while it wasn't running.
        self.wake_held();
        Ok(service)
    }

    /// A plugin's call to the host: a host service its manifest lists in `uses`.
    async fn host_service(
        self: &Arc<Self>,
        plugin: &DiscoveredPlugin,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        if !svc::HOST_SERVICES.contains(&method) {
            return Err(RpcError::method_not_found(method));
        }
        if !plugin.manifest.uses(method) {
            tracing::warn!("`{}` called {method}, which its manifest doesn't list in `uses`", plugin.id());
            return Err(RpcError::not_permitted(format!("{method} isn't in the plugin's `uses`")));
        }
        let plugin_id = plugin.id().to_string();
        match method {
            svc::HOST_NOTIFY => {
                let notify: svc::Notify = rpc::params(params)?;
                self.emit(HostEvent::Notify { plugin_id, notify });
            }
            svc::HOST_OPEN_URL => {
                let svc::OpenUrl { url } = rpc::params(params)?;
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err(RpcError::invalid_params("only http(s) URLs can be opened"));
                }
                self.emit(HostEvent::OpenUrl { plugin_id, url });
            }
            svc::AGENT_COMPLETE => return rpc::result(self.agent_complete(rpc::params(params)?).await?),
            // Declared, but this Space doesn't offer it.
            _ => return Err(RpcError::method_not_found(method)),
        }
        Ok(serde_json::Value::Null)
    }

    /// Fetch the catalogs `plugin_id` publishes and report them.
    pub(super) async fn refresh_catalogs(self: &Arc<Self>, plugin_id: &str) {
        let Some(service) = self.services.lock().unwrap().get(plugin_id).cloned() else { return };
        for name in service.plugin.manifest.catalogs() {
            let request = svc::CatalogName { name: name.to_string() };
            match service.peer.call::<_, svc::Catalog>(svc::CATALOG_GET, &request, Some(SERVICE_TIMEOUT)).await {
                Ok(catalog) => {
                    self.catalogs.lock().unwrap().insert(name.to_string(), catalog.entries.clone());
                    self.emit(HostEvent::Catalog { name: name.to_string(), entries: catalog.entries });
                }
                Err(e) => tracing::warn!("catalog {name} from `{plugin_id}`: {e}"),
            }
        }
    }

    /// Every set setting of `service` (text values, and secrets read from the Keychain), and
    /// the secrets that couldn't be read.
    pub(super) async fn setting_values(&self, service: &Service) -> (SettingValues, Vec<String>) {
        let mut values = SettingValues::new();
        let mut unreadable = Vec::new();
        let text = self.text_settings.lock().unwrap().get(service.plugin.id()).cloned().unwrap_or_default();
        for field in &service.fields {
            let value = match field.kind {
                SettingKind::Text | SettingKind::Switch | SettingKind::Model => text.get(&field.key).cloned(),
                SettingKind::Secret => {
                    let (secrets, id, key) = (self.options.secrets.clone(), service.plugin.id().to_string(), field.key.clone());
                    match tokio::task::spawn_blocking(move || secrets.get(&id, &key)).await {
                        Ok(Ok(v)) => v,
                        Ok(Err(e)) => {
                            tracing::warn!("{e:#}");
                            unreadable.push(field.key.clone());
                            None
                        }
                        Err(_) => None,
                    }
                }
            };
            if let Some(v) = value {
                values.insert(field.key.clone(), v);
            }
        }
        (values, unreadable)
    }

    pub(super) fn setting_states(&self, service: &Service) -> Vec<SettingState> {
        let text = self.text_settings.lock().unwrap().get(service.plugin.id()).cloned().unwrap_or_default();
        service
            .fields
            .iter()
            .map(|field| {
                let (set, value) = match field.kind {
                    SettingKind::Text | SettingKind::Switch | SettingKind::Model => (text.contains_key(&field.key), text.get(&field.key).cloned()),
                    SettingKind::Secret => {
                        let set = self.options.secrets.get(service.plugin.id(), &field.key).ok().flatten().is_some();
                        (set, None)
                    }
                };
                SettingState { field: field.clone(), set, value }
            })
            .collect()
    }

    /// Call a service, starting it first if needed. Errors are plain messages.
    pub(super) async fn call_service<P: Serialize, R: DeserializeOwned>(
        self: &Arc<Self>,
        plugin_id: &str,
        method: &str,
        params: &P,
        timeout: Duration,
    ) -> Result<R, String> {
        let service = self.service(plugin_id).await?;
        service.peer.call(method, params, Some(timeout)).await.map_err(|e| e.message)
    }

    pub(super) fn crashed(&self, plugin_id: &str) {
        let mut crashes = self.crashes.lock().unwrap();
        let times = crashes.entry(plugin_id.to_string()).or_default();
        times.push_back(Instant::now());
        while times.front().is_some_and(|t| t.elapsed() > CRASH_WINDOW) || times.len() > CRASH_LIMIT {
            times.pop_front();
        }
    }

    pub(super) fn keeps_crashing(&self, plugin_id: &str) -> bool {
        let crashes = self.crashes.lock().unwrap();
        crashes.get(plugin_id).is_some_and(|t| t.iter().filter(|t| t.elapsed() <= CRASH_WINDOW).count() >= CRASH_LIMIT)
    }

    /// Hidden sections from every `context` provider (a failing one is skipped).
    /// Every `context` provider's sections for the next prompt, with the plugin each came from.
    pub(super) async fn context_sections(self: &Arc<Self>, session_id: &str) -> Vec<(String, ContextSection)> {
        let (mode, root, _) = self.session_facts(session_id);
        let request = svc::ContextRequest { session_id: session_id.to_string(), mode, project_root: root };
        let mut sections = Vec::new();
        for plugin in self.providers(CAP_CONTEXT) {
            match self.call_service::<_, svc::ContextSections>(plugin.id(), svc::CONTEXT_GET, &request, SERVICE_TIMEOUT).await
            {
                Ok(s) => sections.extend(s.sections.into_iter().map(|section| (plugin.id().to_string(), section))),
                Err(e) => tracing::warn!("context from `{}`: {e}", plugin.id()),
            }
        }
        sections
    }

    /// `_space/sandbox/confine`: the `sandbox` provider's argv, or `argv` when there is none.
    pub(super) async fn confine(self: &Arc<Self>, req: Confine) -> Result<Vec<String>, String> {
        let Some(plugin) = self.providers(CAP_SANDBOX).into_iter().next() else { return Ok(req.argv) };
        let (mode, root, _) = self.session_facts(&req.session_id);
        let request = svc::ConfineRequest { session_id: req.session_id, mode, project_root: root, argv: req.argv, cwd: req.cwd };
        let confined: svc::Confined = self
            .call_service(plugin.id(), svc::SANDBOX_CONFINE, &request, SERVICE_TIMEOUT)
            .await
            .map_err(|e| format!("{} couldn't prepare the command: {e}", plugin.manifest.name))?;
        if confined.argv.is_empty() {
            return Err(format!("{} returned an empty command", plugin.manifest.name));
        }
        Ok(confined.argv)
    }
}
