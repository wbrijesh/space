//! The plugin registry: discovered plugins, which are turned off or not approved, who provides
//! what, and who needs what (`requires`).

use std::collections::HashSet;
use std::sync::Weak;

use space_protocol::service::CatalogEntry;

use super::{Host, HostInner, PluginStatus};
use crate::discovery::{self, DiscoveredPlugin, DiscoveryReport};
use crate::grants::Approval;

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
        let was = std::mem::replace(&mut *self.inner.disabled.lock().unwrap(), ids.clone());
        // Turned back on: it may start at once, without the restart backoff.
        let mut started = self.inner.started_at.lock().unwrap();
        for id in was.difference(&ids) {
            started.remove(id);
        }
        drop(started);
        for id in ids {
            self.inner.stop_service(&id);
        }
    }

    pub fn is_disabled(&self, plugin_id: &str) -> bool {
        self.inner.disabled.lock().unwrap().contains(plugin_id)
    }

    /// Whether the plugin may start (`Pending` for an unknown id).
    pub fn approval(&self, plugin_id: &str) -> Approval {
        self.plugin(plugin_id).map_or(Approval::Pending, |p| self.inner.grants.approval(&p))
    }

    /// Approve or deny the plugin's current permissions (saved). An approved service starts
    /// with the next [`Host::start_services`]; an approved agent with the next start.
    pub fn answer_approval(&self, plugin_id: &str, approved: bool) -> anyhow::Result<()> {
        let plugin = self.plugin(plugin_id).ok_or_else(|| anyhow::anyhow!("unknown plugin `{plugin_id}`"))?;
        self.inner.grants.answer(&plugin, approved)?;
        let status = match self.inner.grants.approval(&plugin).refusal() {
            Some(why) => PluginStatus::NotStarted(why.into()),
            None => PluginStatus::Stopped,
        };
        self.inner.emit_status(plugin_id, status);
        Ok(())
    }

    /// The entries of catalog `name` (empty until its provider has started).
    pub fn catalog(&self, name: &str) -> Vec<CatalogEntry> {
        self.inner.catalogs.lock().unwrap().get(name).cloned().unwrap_or_default()
    }
}

impl HostInner {
    /// Services that are on and approved, in discovery order.
    pub(super) fn enabled_services(&self) -> Vec<DiscoveredPlugin> {
        self.enabled_plugins().into_iter().filter(|p| !p.manifest.is_agent()).collect()
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

    pub(super) fn plugin(&self, plugin_id: &str) -> Option<DiscoveredPlugin> {
        self.plugins.lock().unwrap().iter().find(|p| p.id() == plugin_id).cloned()
    }

    /// Plugins that are on and approved, agents included, in discovery order. The rest count
    /// as not installed: they provide nothing and listen to nothing.
    fn enabled_plugins(&self) -> Vec<DiscoveredPlugin> {
        let disabled = self.disabled.lock().unwrap().clone();
        let plugins = self.plugins.lock().unwrap().clone();
        plugins.into_iter().filter(|p| !disabled.contains(p.id()) && self.grants.approval(p).allows_start()).collect()
    }

    /// Enabled plugins that need something `plugin` provides.
    pub(super) fn dependents(&self, plugin: &DiscoveredPlugin) -> Vec<DiscoveredPlugin> {
        let needs = |p: &DiscoveredPlugin| p.manifest.requires.iter().any(|c| plugin.manifest.provides(c));
        self.enabled_plugins().into_iter().filter(needs).collect()
    }

    /// Whether the plugin's process is up.
    pub(super) fn is_running(&self, plugin: &DiscoveredPlugin) -> bool {
        if plugin.manifest.is_agent() {
            self.agents.lock().unwrap().get(plugin.id()).and_then(Weak::upgrade).is_some_and(|a| a.is_running())
        } else {
            self.services.lock().unwrap().get(plugin.id()).is_some_and(|s| s.alive())
        }
    }

    /// Why `plugin` can't run at all: you haven't approved it, something it requires that no
    /// enabled plugin provides, or requirements that lead back to itself.
    pub(super) fn unmet_requirement(&self, plugin: &DiscoveredPlugin) -> Option<String> {
        if let Some(why) = self.grants.approval(plugin).refusal() {
            return Some(why.into());
        }
        if let Some(cap) = plugin.manifest.requires.iter().find(|c| self.providers(c).is_empty()) {
            return Some(format!("Needs a {cap} provider."));
        }
        let mut path = vec![plugin.manifest.name.clone()];
        self.leads_back(plugin, plugin.id(), &mut path, &mut HashSet::new())
            .then(|| format!("Its requirements go in a circle: {}.", path.join(" → ")))
    }

    /// Whether `start` is reachable from `plugin` through requirements (`path`: the names on
    /// the way).
    fn leads_back(&self, plugin: &DiscoveredPlugin, start: &str, path: &mut Vec<String>, seen: &mut HashSet<String>) -> bool {
        for provider in plugin.manifest.requires.iter().flat_map(|c| self.providers(c)) {
            path.push(provider.manifest.name.clone());
            if provider.id() == start || (seen.insert(provider.id().to_string()) && self.leads_back(&provider, start, path, seen)) {
                return true;
            }
            path.pop();
        }
        false
    }
}
