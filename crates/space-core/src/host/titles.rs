//! Thread titles (the `session-title` seam) and `agent/complete`, the host service a title
//! provider asks the thread's agent model with.

use std::sync::{Arc, Weak};
use std::time::Duration;

use space_plugin::rpc::RpcError;
use space_protocol::service::{self as svc, Complete, CompleteResult, Observed};
use space_protocol::CAP_SESSION_TITLE;

use super::{Host, HostInner};

/// How long a thread waits for its generated title before keeping the first-line one.
pub const TITLE_TIMEOUT: Duration = Duration::from_secs(15);
/// One `agent/complete` call.
const COMPLETE_TIMEOUT: Duration = Duration::from_secs(60);

impl Host {
    /// You renamed a thread: plugins that follow titles hear of it as they do of a generated one.
    pub fn thread_renamed(&self, session_id: &str, title: &str) {
        self.inner.observe(Observed::ThreadTitled { session_id: session_id.to_string(), title: Some(title.to_string()) });
    }

    /// Ask the `session-title` provider to name the thread of `session_id` after its first
    /// message, waiting at most `timeout`. `None`: no provider, or it failed or took too long
    /// (the thread keeps its first-line title). A title is told to observers (`thread/titled`).
    pub async fn generate_title(&self, session_id: &str, first_message: &str, timeout: Duration) -> Option<String> {
        let plugin = self.inner.providers(CAP_SESSION_TITLE).into_iter().next()?;
        let (project_root, inner) = (self.inner.session_facts(session_id).1, self.inner.clone());
        let request = svc::TitleRequest {
            session_id: session_id.to_string(),
            first_message: first_message.to_string(),
            project_root,
        };
        let id = plugin.id().to_string();
        let result = self
            .inner
            .rt
            .spawn(async move { inner.call_service::<_, svc::Title>(&id, svc::TITLE_GENERATE, &request, timeout).await })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
        let title = match result {
            Ok(t) => t.title.trim().to_string(),
            Err(e) => {
                tracing::info!("no title from `{}`: {e}", plugin.id());
                return None;
            }
        };
        if title.is_empty() {
            return None;
        }
        self.inner.observe(Observed::ThreadTitled { session_id: session_id.to_string(), title: Some(title.clone()) });
        Some(title)
    }
}

impl HostInner {
    /// `agent/complete`: forward to the agent of the request's session.
    pub(super) async fn agent_complete(self: &Arc<Self>, request: Complete) -> Result<CompleteResult, RpcError> {
        let agent_id = self.sessions.lock().unwrap().get(&request.session_id).map(|m| m.agent.clone());
        let agent_id = agent_id.ok_or_else(|| RpcError::invalid_params(format!("no session {}", request.session_id)))?;
        let agent = self.agents.lock().unwrap().get(&agent_id).and_then(Weak::upgrade);
        let agent = agent.filter(|a| a.is_running()).ok_or_else(|| RpcError::unavailable(format!("`{agent_id}` isn't running")))?;
        match tokio::time::timeout(COMPLETE_TIMEOUT, agent.complete(request)).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(e)) => Err(RpcError::internal(format!("{e:#}"))),
            Err(_) => Err(RpcError::unavailable("the agent took too long")),
        }
    }
}
