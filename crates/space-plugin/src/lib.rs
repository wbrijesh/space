//! Write a Space service plugin in Rust.
//!
//! A service plugin is a process the host talks to over JSON-RPC on stdio (see
//! [`space_protocol::service`]). Implement [`Plugin`] for the capabilities your manifest
//! declares and call [`run`] from `main`:
//!
//! ```no_run
//! use space_plugin::{Plugin, protocol::service::*};
//!
//! struct Audit;
//! impl Plugin for Audit {
//!     async fn pre_execute(&self, call: HookCall) -> Result<HookDecision, space_plugin::RpcError> {
//!         eprintln!("{} {}", call.call.tool, call.call.args);
//!         Ok(HookDecision::next())
//!     }
//! }
//!
//! fn main() {
//!     space_plugin::run(Audit);
//! }
//! ```
//!
//! stdout carries protocol messages only; log to stderr (`eprintln!`), which the host shows in
//! its logs panel.

pub mod rpc;

use std::future::Future;
use std::sync::{Arc, OnceLock};

use serde_json::Value;
use space_protocol::service::*;
use tokio::io::{AsyncRead, AsyncWrite};

pub use rpc::{Peer, RpcError};
pub use space_protocol as protocol;

/// What a service plugin does. Every method has a default, so implement only what the
/// manifest declares. Methods may run concurrently.
pub trait Plugin: Send + Sync + 'static {
    /// The first call, with the plugin's current settings. `host` stays valid for the life of
    /// the connection (keep it to send [`Host::catalog_changed`]).
    fn initialize(&self, init: Initialize, host: Host) -> impl Future<Output = Result<Initialized, RpcError>> + Send {
        let _ = (init, host);
        async { Ok(Initialized::default()) }
    }

    /// `provides = ["settings"]`: the fields the Settings page shows.
    fn settings_schema(&self) -> SettingsSchema {
        SettingsSchema::default()
    }

    /// `provides = ["settings"]`: check a value before the host stores it (e.g. try an API
    /// key). Accepts everything by default.
    fn validate_setting(&self, check: SettingCheck) -> impl Future<Output = Result<SettingVerdict, RpcError>> + Send {
        let _ = check;
        async { Ok(SettingVerdict::ok()) }
    }

    /// The user changed a setting; `changed.values` holds every setting's current value.
    fn settings_changed(&self, changed: SettingsChanged) {
        let _ = changed;
    }

    /// The user pressed "Retry now" on one of the plugin's blockers.
    fn retry(&self) {}

    /// `provides = ["catalog:<name>"]`: the named catalog's entries.
    fn catalog(&self, name: &str) -> Result<Catalog, RpcError> {
        Err(RpcError::method_not_found(&format!("{CATALOG_GET} {name}")))
    }

    /// `provides = ["context"]`: hidden sections for the next prompt.
    fn context(&self, request: ContextRequest) -> impl Future<Output = Result<ContextSections, RpcError>> + Send {
        let _ = request;
        async { Ok(ContextSections::default()) }
    }

    /// `provides = ["sandbox"]`: the argv to spawn instead (unchanged by default).
    fn confine(&self, request: ConfineRequest) -> impl Future<Output = Result<Confined, RpcError>> + Send {
        async move { Ok(Confined { argv: request.argv }) }
    }

    /// An event the manifest `observes`. Events come one at a time, in order; the next waits
    /// until this returns, so a slow one only delays (and can make the host drop) the plugin's
    /// own events.
    fn observe(&self, event: Observed) -> impl Future<Output = ()> + Send {
        let _ = event;
        async {}
    }

    /// `provides = ["session-store"]`: every saved thread, newest first.
    fn store_list(&self) -> impl Future<Output = Result<StoredThreads, RpcError>> + Send {
        async { Err(RpcError::method_not_found(STORE_LIST)) }
    }

    /// `provides = ["session-store"]`: a thread's summary and events.
    fn store_load(&self, thread: ThreadRef) -> impl Future<Output = Result<StoredThread, RpcError>> + Send {
        let _ = thread;
        async { Err(RpcError::method_not_found(STORE_LOAD)) }
    }

    /// `provides = ["session-store"]`: append events to a thread and replace its summary.
    fn store_write(&self, write: StoreWrite) -> impl Future<Output = Result<(), RpcError>> + Send {
        let _ = write;
        async { Err(RpcError::method_not_found(STORE_WRITE)) }
    }

    /// `provides = ["session-store"]`: delete a thread and its agent's files.
    fn store_delete(&self, thread: ThreadRef) -> impl Future<Output = Result<(), RpcError>> + Send {
        let _ = thread;
        async { Err(RpcError::method_not_found(STORE_DELETE)) }
    }

    /// `provides = ["session-store"]`: the folder the thread's agent may keep files in.
    fn store_data_dir(&self, thread: ThreadRef) -> impl Future<Output = Result<DataDir, RpcError>> + Send {
        let _ = thread;
        async { Err(RpcError::method_not_found(STORE_DATA_DIR)) }
    }

    /// Hook `tools/pre-execute`: a verdict on the pending tool call.
    fn pre_execute(&self, call: HookCall) -> impl Future<Output = Result<HookDecision, RpcError>> + Send {
        let _ = call;
        async { Ok(HookDecision::next()) }
    }
}

/// The plugin's handle on the host.
#[derive(Clone)]
pub struct Host {
    peer: Peer,
}

impl Host {
    /// Tell the host a catalog's entries changed; it fetches them again.
    pub fn catalog_changed(&self, name: &str) {
        self.peer.notify(CATALOG_CHANGED, CatalogName { name: name.to_string() });
    }

    /// Tell the host the plugin's hooks can decide again (after answering `unavailable`); it
    /// runs the calls it held through the chain again.
    pub fn hooks_ready(&self) {
        self.peer.notify(HOOKS_READY, serde_json::json!({}));
    }

    /// Call a host service (one the manifest lists in `uses`; others fail with
    /// [`RpcError::NOT_PERMITTED`]).
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.peer.request(method, params, None).await
    }
}

/// Serve `plugin` on `reader`/`writer` until `reader` closes.
pub async fn serve<P, R, W>(plugin: P, reader: R, writer: W)
where
    P: Plugin,
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let plugin = Arc::new(plugin);
    let host: Arc<OnceLock<Host>> = Arc::new(OnceLock::new());
    let on_request: rpc::RequestHandler = {
        let (plugin, host) = (plugin.clone(), host.clone());
        Arc::new(move |method: String, params: Value| {
            let (plugin, host) = (plugin.clone(), host.clone());
            Box::pin(async move { handle(&*plugin, &host, &method, params).await })
        })
    };
    let on_notification: rpc::NotificationHandler = {
        let plugin = plugin.clone();
        Arc::new(move |method: String, params: Value| match method.as_str() {
            SETTINGS_CHANGED => match rpc::params::<SettingsChanged>(params) {
                Ok(changed) => plugin.settings_changed(changed),
                Err(e) => eprintln!("space-plugin: bad {SETTINGS_CHANGED}: {e}"),
            },
            PLUGIN_RETRY => plugin.retry(),
            _ => {}
        })
    };
    let (peer, done) = rpc::Peer::spawn(reader, writer, on_request, on_notification);
    let _ = host.set(Host { peer });
    let _ = done.await;
}

async fn handle<P: Plugin>(plugin: &P, host: &OnceLock<Host>, method: &str, params: Value) -> Result<Value, RpcError> {
    match method {
        INITIALIZE => {
            let init: Initialize = rpc::params(params)?;
            let host = host.get().cloned().ok_or_else(|| RpcError::internal("not connected"))?;
            let initialized = plugin.initialize(init, host).await?;
            rpc::result(Initialized { protocol_version: Some(PROTOCOL_VERSION), ..initialized })
        }
        SETTINGS_SCHEMA => rpc::result(plugin.settings_schema()),
        SETTINGS_VALIDATE => rpc::result(plugin.validate_setting(rpc::params(params)?).await?),
        CATALOG_GET => rpc::result(plugin.catalog(&rpc::params::<CatalogName>(params)?.name)?),
        CONTEXT_GET => rpc::result(plugin.context(rpc::params(params)?).await?),
        SANDBOX_CONFINE => rpc::result(plugin.confine(rpc::params(params)?).await?),
        HOOK_PRE_EXECUTE => rpc::result(plugin.pre_execute(rpc::params(params)?).await?),
        STORE_LIST => rpc::result(plugin.store_list().await?),
        STORE_LOAD => rpc::result(plugin.store_load(rpc::params(params)?).await?),
        STORE_WRITE => rpc::result(plugin.store_write(rpc::params(params)?).await?),
        STORE_DELETE => rpc::result(plugin.store_delete(rpc::params(params)?).await?),
        STORE_DATA_DIR => rpc::result(plugin.store_data_dir(rpc::params(params)?).await?),
        EVENTS_OBSERVE => {
            plugin.observe(rpc::params(params)?).await;
            rpc::result(())
        }
        _ => Err(RpcError::method_not_found(method)),
    }
}

/// Serve `plugin` on stdin/stdout until the host closes stdin, then exit.
pub fn run<P: Plugin>(plugin: P) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(serve(plugin, tokio::io::stdin(), tokio::io::stdout()));
}

/// Test helpers: run a plugin in-process and talk to it as the host would.
pub mod testing {
    use super::*;

    /// Serve `plugin` over an in-memory pipe; returns the host's end. `notifications` gets every
    /// notification the plugin sends.
    pub fn connect<P: Plugin>(plugin: P, notifications: rpc::NotificationHandler) -> Peer {
        let (host_side, plugin_side) = tokio::io::duplex(1 << 20);
        let (pr, pw) = tokio::io::split(plugin_side);
        tokio::spawn(serve(plugin, pr, pw));
        let (hr, hw) = tokio::io::split(host_side);
        let no_requests: rpc::RequestHandler =
            Arc::new(|m: String, _| Box::pin(async move { Err(RpcError::method_not_found(&m)) }));
        rpc::Peer::spawn(hr, hw, no_requests, notifications).0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Demo {
        settings: Mutex<SettingValues>,
        host: OnceLock<Host>,
    }

    impl Plugin for Demo {
        async fn initialize(&self, init: Initialize, host: Host) -> Result<Initialized, RpcError> {
            *self.settings.lock().unwrap() = init.settings;
            let _ = self.host.set(host);
            Ok(Initialized { name: Some("demo".into()), ..Default::default() })
        }
        fn settings_changed(&self, changed: SettingsChanged) {
            *self.settings.lock().unwrap() = changed.values;
            if let Some(h) = self.host.get() {
                h.catalog_changed("modes");
            }
        }
        fn catalog(&self, name: &str) -> Result<Catalog, RpcError> {
            let ready = self.settings.lock().unwrap().contains_key("key");
            Ok(Catalog { entries: vec![CatalogEntry { id: name.into(), name: "M".into(), ready, ..Default::default() }] })
        }
        async fn pre_execute(&self, call: HookCall) -> Result<HookDecision, RpcError> {
            Ok(if call.call.tool == "rm" { HookDecision::deny("no") } else { HookDecision::next() })
        }
    }

    #[tokio::test]
    async fn serves_the_protocol() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let host = testing::connect(Demo::default(), Arc::new(move |m, p| s.lock().unwrap().push((m, p))));
        let init: Initialized =
            host.call(INITIALIZE, &Initialize { protocol_version: PROTOCOL_VERSION, ..Default::default() }, None).await.unwrap();
        assert_eq!(init.name.as_deref(), Some("demo"));
        assert_eq!(init.protocol_version, Some(PROTOCOL_VERSION));
        let cat: Catalog = host.call(CATALOG_GET, &CatalogName { name: "modes".into() }, None).await.unwrap();
        assert!(!cat.entries[0].ready);

        host.notify(SETTINGS_CHANGED, SettingsChanged { values: [("key".into(), "v".into())].into(), ..Default::default() });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(seen.lock().unwrap()[0].0, CATALOG_CHANGED);
        let cat: Catalog = host.call(CATALOG_GET, &CatalogName { name: "modes".into() }, None).await.unwrap();
        assert!(cat.entries[0].ready);

        let call = |tool: &str| HookCall {
            call: ToolCallFacts { tool: tool.into(), ..Default::default() },
            ..Default::default()
        };
        let d: HookDecision = host.call(HOOK_PRE_EXECUTE, &call("rm"), None).await.unwrap();
        assert_eq!(d.decision, HookVerdict::Deny);
        let d: HookDecision = host.call(HOOK_PRE_EXECUTE, &call("ls"), None).await.unwrap();
        assert_eq!(d.decision, HookVerdict::Next);
        // Defaults: confine passes the argv through; unknown methods fail.
        let c: Confined = host
            .call(SANDBOX_CONFINE, &ConfineRequest { argv: vec!["ls".into()], ..Default::default() }, None)
            .await
            .unwrap();
        assert_eq!(c.argv, ["ls"]);
        assert_eq!(host.request("nope", Value::Null, None).await.unwrap_err().code, RpcError::METHOD_NOT_FOUND);
        // Settings are accepted by default.
        let v: SettingVerdict = host
            .call(SETTINGS_VALIDATE, &SettingCheck { key: "key".into(), value: "v".into() }, None)
            .await
            .unwrap();
        assert_eq!(v, SettingVerdict::ok());
    }
}
