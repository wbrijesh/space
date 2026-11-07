//! The plugin registry: discovered plugins, which are turned off, and who provides what.

use std::collections::HashSet;

use space_protocol::service::CatalogEntry;

use super::{Host, HostInner};
use crate::discovery::{self, DiscoveredPlugin, DiscoveryReport};

impl Host {
    /// Re-scan plugin directories, replacing the registry. Invalid manifests are returned
    /// in the report (and logged) rather than failing discovery.
    pub fn discover(&self) -> DiscoveryReport {
        let report = discovery::discover(&self.inner.options.discovery);
        for e in &report.errors {
            tracing::warn!("{e}");
        }
        *self.inner.plugins.lock().unwrap() = report.plugins.clone();
        report
    }

    pub fn plugins(&self) -> Vec<DiscoveredPlugin> {
        self.inner.plugins.lock().unwrap().clone()
    }

    pub fn plugin(&self, id: &str) -> Option<DiscoveredPlugin> {
        self.inner.plugins.lock().unwrap().iter().find(|p| p.id() == id).cloned()
    }

    /// Service plugins (not agents), in discovery order.
    pub fn service_plugins(&self) -> Vec<DiscoveredPlugin> {
        self.plugins().into_iter().filter(|p| !p.manifest.is_agent()).collect()
    }

    /// Turn plugins off (by id); every other plugin is on. Disabled services stop at once,
    /// and their hooks, seams, catalogs and context go with them. `required` plugins stay on.
    pub fn set_disabled(&self, ids: impl IntoIterator<Item = String>) {
        let required: HashSet<String> =
            self.plugins().into_iter().filter(|p| p.manifest.required).map(|p| p.id().to_string()).collect();
        let ids: HashSet<String> = ids.into_iter().filter(|id| !required.contains(id)).collect();
        *self.inner.disabled.lock().unwrap() = ids.clone();
        for id in ids {
            self.inner.stop_service(&id);
        }
    }

    pub fn is_disabled(&self, plugin_id: &str) -> bool {
        self.inner.disabled.lock().unwrap().contains(plugin_id)
    }

    /// The entries of catalog `name` (empty until its provider has started).
    pub fn catalog(&self, name: &str) -> Vec<CatalogEntry> {
        self.inner.catalogs.lock().unwrap().get(name).cloned().unwrap_or_default()
    }
}

impl HostInner {
    pub(super) fn enabled_services(&self) -> Vec<DiscoveredPlugin> {
        let disabled = self.disabled.lock().unwrap().clone();
        let plugins = self.plugins.lock().unwrap();
        plugins.iter().filter(|p| !p.manifest.is_agent() && !disabled.contains(p.id())).cloned().collect()
    }

    /// Enabled services listening to `event`, in chain order.
    pub(super) fn hook_listeners(&self, event: &str) -> Vec<DiscoveredPlugin> {
        let mut listeners: Vec<(i32, DiscoveredPlugin)> = self
            .enabled_services()
            .into_iter()
            .filter_map(|p| p.manifest.hook_order(event).map(|o| (o, p)))
            .collect();
        listeners.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id().cmp(b.1.id())));
        listeners.into_iter().map(|(_, p)| p).collect()
    }

    /// The enabled services providing `capability`, in discovery order.
    pub(super) fn providers(&self, capability: &str) -> Vec<DiscoveredPlugin> {
        self.enabled_services().into_iter().filter(|p| p.manifest.provides(capability)).collect()
    }
}
