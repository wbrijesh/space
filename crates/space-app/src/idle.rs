//! The agent goes to sleep after a while with nothing to do: its process stops (with pi-agent's
//! Claude CLI processes), each thread keeps its session to load again, and the agent starts
//! again when you write (typing starts it early) or send.

use std::time::{Duration, Instant};

use gpui::*;

use crate::app::SpaceApp;

/// How long the agent may sit idle before it's stopped.
pub const AGENT_IDLE: Duration = Duration::from_secs(30 * 60);
/// Overrides [`AGENT_IDLE`], in seconds (tests, scenarios).
pub const AGENT_IDLE_ENV: &str = "SPACE_AGENT_IDLE_SECS";

pub fn idle_limit() -> Duration {
    std::env::var(AGENT_IDLE_ENV).ok().and_then(|v| v.parse::<f64>().ok()).filter(|s| *s > 0.).map_or(AGENT_IDLE, Duration::from_secs_f64)
}

/// How often the idle check runs: once a minute, more often for a short limit.
pub fn check_every(limit: Duration) -> Duration {
    (limit / 4).clamp(Duration::from_millis(250), Duration::from_secs(60))
}

/// What one thread is doing, for the idle check.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadActivity {
    pub running: bool,
    /// A message waits for its session.
    pub queued: bool,
    /// Its session is being created or loaded.
    pub creating: bool,
    /// A permission card waits for the user, or a tool call is held by a guard.
    pub waiting: bool,
}

impl ThreadActivity {
    pub fn busy(&self) -> bool {
        self.running || self.queued || self.creating || self.waiting
    }
}

/// Whether the agent should be stopped now: nothing is busy and nothing happened for `limit`.
pub fn should_sleep(threads: impl IntoIterator<Item = ThreadActivity>, last_activity: Instant, now: Instant, limit: Duration) -> bool {
    !threads.into_iter().any(|t| t.busy()) && now.saturating_duration_since(last_activity) >= limit
}

impl SpaceApp {
    /// Something happened that keeps the agent awake (a session event, a message, an answer, a
    /// model or mode change).
    pub(crate) fn touch_agent(&mut self) {
        self.agent_activity = Instant::now();
    }

    /// Whether anything keeps the agent busy (a thread running, queued, starting, or waiting on
    /// you or the guardrail). Plugin reload (`just run`) waits for it before restarting the agent.
    #[cfg(feature = "dev")]
    pub(crate) fn agent_busy(&self) -> bool {
        self.threads.iter().any(|t| {
            ThreadActivity {
                running: t.state.running,
                queued: t.queued.is_some() || !t.pending.is_empty(),
                creating: t.creating,
                waiting: t.state.pending_permission().is_some() || t.state.held().is_some(),
            }
            .busy()
        })
    }

    /// Check now and then whether the agent has been idle long enough to stop.
    pub(crate) fn watch_agent_idle(&mut self, cx: &mut Context<Self>) {
        let limit = idle_limit();
        let every = check_every(limit);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(every).await;
                let Ok(()) = this.update(cx, |this, cx| this.sleep_if_idle(limit, cx)) else { break };
            }
        })
        .detach();
    }

    fn sleep_if_idle(&mut self, limit: Duration, cx: &mut Context<Self>) {
        if !self.agent_running() {
            return;
        }
        let threads = self.threads.iter().map(|t| ThreadActivity {
            running: t.state.running,
            queued: t.queued.is_some(),
            creating: t.creating,
            waiting: t.state.pending_permission().is_some() || t.state.held().is_some(),
        });
        if should_sleep(threads, self.agent_activity, Instant::now(), limit) {
            self.sleep_agent(cx);
        }
    }

    /// Stop the agent; each thread keeps its session to load again when it's used.
    pub(crate) fn sleep_agent(&mut self, cx: &mut Context<Self>) {
        let Some(agent) = self.agent.take() else { return };
        tracing::info!("the agent was idle for {:?}: stopping it", idle_limit());
        for t in &mut self.threads {
            if let Some(old) = t.session.take() {
                t.resume = Some((old.agent().plugin_id().to_string(), old.id().to_string()));
            }
        }
        self.agent_asleep = true;
        agent.stop();
        self.agent_status = space_core::PluginStatus::Stopped;
        cx.notify();
    }

    /// The composer got text while the agent sleeps: start it now so it's up when you send.
    pub(crate) fn wake_agent(&mut self, cx: &mut Context<Self>) {
        if self.agent_asleep && !self.agent_running() {
            self.ensure_agent(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings in gpui's `test` attribute.
    use std::time::{Duration, Instant};

    use super::{AGENT_IDLE, ThreadActivity, check_every, should_sleep};

    #[test]
    fn sleeps_only_when_nothing_is_busy_for_the_whole_limit() {
        let limit = Duration::from_secs(1800);
        let t0 = Instant::now();
        let idle = ThreadActivity::default();
        let later = t0 + limit;
        assert!(should_sleep([idle, idle], t0, later, limit));
        assert!(!should_sleep([idle], t0, later - Duration::from_secs(1), limit), "not yet");
        // Activity resets the clock.
        assert!(!should_sleep([idle], t0 + Duration::from_secs(60), later, limit));
        // Any busy thread keeps it awake, however long ago the last event was.
        for busy in [
            ThreadActivity { running: true, ..idle },
            ThreadActivity { queued: true, ..idle },
            ThreadActivity { creating: true, ..idle },
            ThreadActivity { waiting: true, ..idle },
        ] {
            assert!(!should_sleep([idle, busy], t0, later + limit, limit), "{busy:?}");
        }
        // No threads at all: nothing keeps it awake.
        assert!(should_sleep([], t0, later, limit));
    }

    #[test]
    fn the_check_runs_once_a_minute_or_more_often_for_short_limits() {
        assert_eq!(check_every(AGENT_IDLE), Duration::from_secs(60));
        assert_eq!(check_every(Duration::from_secs(2)), Duration::from_millis(500));
        assert_eq!(check_every(Duration::from_millis(100)), Duration::from_millis(250));
    }
}
