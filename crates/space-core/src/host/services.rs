//! Service plugins: starting and restarting them, their settings and catalogs, and the seams
//! the host calls (`context`, `sandbox`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde::de::DeserializeOwned;
use space_plugin::rpc::{self, Peer, RpcError};
use space_protocol::service::{self as svc, SettingField, SettingKind, SettingValues};
use space_protocol::{CAP_CONTEXT, CAP_SANDBOX, CAP_SETTINGS, Confine, ContextSection};

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
    plugin: DiscoveredPlugin,
    peer: Peer,
    _process: PluginProcess,
    dead: Arc<AtomicBool>,
    fields: Vec<SettingField>,
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
                let ids: Vec<String> = inner.enabled_services().into_iter().map(|p| p.id().to_string()).collect();
                futures::future::join_all(ids.iter().map(|id| inner.service(id))).await;
            })
            .await;
    }

    /// Stop and start a service plugin again.
    pub async fn restart_service(&self, plugin_id: &str) -> Result<()> {
        self.inner.stop_service(plugin_id);
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
                    SettingKind::Text => {
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
    pub(super) fn stop_service(&self, plugin_id: &str) {
        let Some(service) = self.services.lock().unwrap().remove(plugin_id) else { return };
        service._process.kill();
        let mut catalogs = self.catalogs.lock().unwrap();
        for name in service.plugin.manifest.catalogs() {
            if catalogs.remove(name).is_some() {
                self.emit(HostEvent::Catalog { name: name.to_string(), entries: Vec::new() });
            }
        }
    }

    /// The running service `plugin_id`, started (or restarted) first if needed.
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
                    if let Some(host) = exit_weak.upgrade() {
                        let status = exit_status(kind, &tail);
                        if matches!(status, PluginStatus::Crashed(_)) {
                            host.crashed(&exit_id);
                        }
                        host.emit_exit(&exit_id, status);
                    }
                },
            )?
        };
        let on_request: rpc::RequestHandler =
            Arc::new(|method: String, _| Box::pin(async move { Err(RpcError::method_not_found(&method)) }));
        let on_notification: rpc::NotificationHandler = {
            let (weak, id) = (Arc::downgrade(self), plugin_id.clone());
            Arc::new(move |method: String, _params| {
                let Some(host) = weak.upgrade() else { return };
                match method.as_str() {
                    svc::CATALOG_CHANGED => {
                        let id = id.clone();
                        host.rt.clone().spawn(async move { host.refresh_catalogs(&id).await });
                    }
                    svc::HOOKS_READY => host.wake_held(),
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
        let mut service = Service { plugin, peer, _process: process, dead, fields };
        let (settings, unreadable) = self.setting_values(&service).await;
        let init = svc::Initialize { protocol_version: svc::PROTOCOL_VERSION, settings, unreadable };
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
                SettingKind::Text => text.get(&field.key).cloned(),
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
                    SettingKind::Text => (text.contains_key(&field.key), text.get(&field.key).cloned()),
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
    pub(super) async fn context_sections(self: &Arc<Self>, session_id: &str) -> Vec<ContextSection> {
        let (mode, root, _) = self.session_facts(session_id);
        let request = svc::ContextRequest { session_id: session_id.to_string(), mode, project_root: root };
        let mut sections = Vec::new();
        for plugin in self.providers(CAP_CONTEXT) {
            match self.call_service::<_, svc::ContextSections>(plugin.id(), svc::CONTEXT_GET, &request, SERVICE_TIMEOUT).await
            {
                Ok(s) => sections.extend(s.sections),
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
