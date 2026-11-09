//! The terminal seam: the `terminal` provider runs shells; the app draws them.
//!
//! The app's calls ([`Host::terminal_open`], ...) go to the provider. Output comes back as
//! `terminal/output` notifications, in order, on the plugin's connection; each is handed to the
//! app's [`TerminalSink`] right there (off the UI thread, so a flood of output is parsed
//! without holding up drawing), and when the sink says so the app is told to draw again
//! ([`HostEvent::TerminalOutput`]). Titles and exits go to the app as events.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde::de::DeserializeOwned;
use space_plugin::rpc;
use space_protocol::CAP_TERMINAL;
use space_protocol::terminal::*;

use super::{Host, HostEvent, HostInner};

/// Starting a shell can take a moment (a login shell reads its profile only after; this is
/// the PTY and the process).
const TERMINAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Gets every terminal's output, in order, on a background thread: `(terminal id, seq, bytes)`.
/// Returns whether the app should be told to draw (e.g. the terminal is on screen and no
/// redraw is pending yet).
pub type TerminalSink = Arc<dyn Fn(&str, u64, &[u8]) -> bool + Send + Sync>;

impl HostInner {
    fn terminal_plugin(&self) -> Result<String> {
        self.providers(CAP_TERMINAL).into_iter().next().map(|p| p.id().to_string()).ok_or_else(|| anyhow!("no terminal plugin is installed"))
    }

    async fn terminal_call<P: Serialize, R: DeserializeOwned>(self: &Arc<Self>, method: &str, params: &P) -> Result<R> {
        let id = self.terminal_plugin()?;
        self.call_service(&id, method, params, TERMINAL_TIMEOUT).await.map_err(|e| anyhow!("terminal ({method}): {e}"))
    }

    /// A notification to the running provider (dropped when it isn't running).
    fn terminal_notify(&self, method: &str, params: impl Serialize) {
        let Ok(id) = self.terminal_plugin() else { return };
        let service = self.services.lock().unwrap().get(&id).cloned();
        if let Some(service) = service.filter(|s| s.alive()) {
            service.peer.notify(method, params);
        }
    }

    /// `terminal/*` from the provider.
    pub(super) fn terminal_notification(&self, method: &str, params: serde_json::Value) {
        match method {
            TERMINAL_OUTPUT => match rpc::params::<TerminalOutput>(params) {
                Ok(out) => {
                    let sink = self.terminal_sink.lock().unwrap().clone();
                    if sink.is_some_and(|sink| sink(&out.terminal_id, out.seq, &out.data.0)) {
                        self.emit(HostEvent::TerminalOutput { terminal_id: out.terminal_id });
                    }
                }
                Err(e) => tracing::warn!("bad {TERMINAL_OUTPUT}: {e}"),
            },
            TERMINAL_TITLE => match rpc::params::<TerminalTitle>(params) {
                Ok(t) => self.emit(HostEvent::TerminalTitle { terminal_id: t.terminal_id, title: t.title }),
                Err(e) => tracing::warn!("bad {TERMINAL_TITLE}: {e}"),
            },
            TERMINAL_EXITED => match rpc::params::<TerminalExited>(params) {
                Ok(e) => self.emit(HostEvent::TerminalExited { terminal_id: e.terminal_id, exit_code: e.exit_code }),
                Err(e) => tracing::warn!("bad {TERMINAL_EXITED}: {e}"),
            },
            _ => {}
        }
    }
}

impl Host {
    /// Whether a terminal plugin is installed (on and approved): the app shows the terminal.
    pub fn has_terminal(&self) -> bool {
        !self.inner.providers(CAP_TERMINAL).is_empty()
    }

    /// The terminal plugin's id, if one is installed.
    pub fn terminal_plugin(&self) -> Option<String> {
        self.inner.terminal_plugin().ok()
    }

    /// Where terminal output goes from now on.
    pub fn set_terminal_sink(&self, sink: TerminalSink) {
        *self.inner.terminal_sink.lock().unwrap() = Some(sink);
    }

    /// Start a shell for a thread, in `cwd`.
    pub async fn terminal_open(&self, open: TerminalOpen) -> Result<TerminalOpened> {
        let inner = self.inner.clone();
        self.inner.rt.spawn(async move { inner.terminal_call(TERMINAL_OPEN, &open).await }).await?
    }

    /// Keys or a paste for a terminal (in order with earlier input).
    pub fn terminal_input(&self, terminal_id: &str, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        self.inner.terminal_notify(TERMINAL_INPUT, TerminalInput { terminal_id: terminal_id.to_string(), data: Bytes(data) });
    }

    pub fn terminal_resize(&self, terminal_id: &str, cols: u16, rows: u16) {
        self.inner.terminal_notify(TERMINAL_RESIZE, TerminalResize { terminal_id: terminal_id.to_string(), cols, rows });
    }

    /// Everything a terminal sent that's still kept, to draw it from scratch.
    pub async fn terminal_attach(&self, terminal_id: &str) -> Result<TerminalReplay> {
        let inner = self.inner.clone();
        let terminal = TerminalRef { terminal_id: terminal_id.to_string() };
        self.inner.rt.spawn(async move { inner.terminal_call(TERMINAL_ATTACH, &terminal).await }).await?
    }

    /// The terminals the plugin runs.
    pub async fn terminal_list(&self) -> Result<Vec<TerminalInfo>> {
        let inner = self.inner.clone();
        let list: TerminalList =
            self.inner.rt.spawn(async move { inner.terminal_call(TERMINAL_LIST, &serde_json::json!({})).await }).await??;
        Ok(list.terminals)
    }

    /// Stop a terminal's shell and everything it started.
    pub async fn terminal_close(&self, terminal_id: &str) -> Result<()> {
        let inner = self.inner.clone();
        let terminal = TerminalRef { terminal_id: terminal_id.to_string() };
        self.inner.rt.spawn(async move { inner.terminal_call(TERMINAL_CLOSE, &terminal).await }).await?
    }
}
