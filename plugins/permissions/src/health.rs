//! Whether the decision model can be used right now, why not, and when it's tried again.
//!
//! One [`Health`] value per plugin, owned by `main.rs`. Every review and probe feeds it a
//! result; it answers what a review should do, what the catalog blockers say, and when the
//! next probe is due. Times are passed in, so the transitions are tested without a clock.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Probes are this far apart while the model can't be used (`SPACE_GUARD_RETRY_SECS`
/// overrides it, for tests).
pub const RETRY_INTERVAL: Duration = Duration::from_secs(300);
pub const RETRY_ENV: &str = "SPACE_GUARD_RETRY_SECS";
/// This many unreadable answers in a row and the API is taken to have changed.
pub const MALFORMED_LIMIT: u32 = 3;
pub const BILLING_URL: &str = "https://platform.openai.com/settings/organization/billing";

pub fn retry_interval() -> Duration {
    std::env::var(RETRY_ENV).ok().and_then(|v| v.parse::<f64>().ok()).map_or(RETRY_INTERVAL, Duration::from_secs_f64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No key (or the Keychain wouldn't give it).
    NotSet,
    /// A key was just set or the plugin started: the first probe is under way.
    Checking,
    Healthy,
    /// Timeouts, network errors, 5xx, rate limits, unreadable answers.
    Unreachable,
    /// 401.
    KeyRejected,
    /// 403, or the endpoint isn't there for this account.
    NoAccess,
    /// 429 `insufficient_quota`.
    NoCredit,
    /// The API no longer takes our request (400, model gone, answers we can't read).
    Incompatible,
}

/// Why a call to the Decisions API failed, classified.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub state: State,
    /// Short, for the blocker ("timed out", "HTTP 503", the API's message).
    pub detail: String,
    /// The server asked us to wait at least this long (`Retry-After`).
    pub retry_after: Option<Duration>,
    /// The answer couldn't be read (counts toward [`MALFORMED_LIMIT`]).
    pub malformed: bool,
}

impl Failure {
    pub fn new(state: State, detail: impl Into<String>) -> Self {
        Self { state, detail: detail.into(), retry_after: None, malformed: false }
    }

    /// Worth retrying at once within the same review (a blip), as opposed to a state that
    /// needs the user or a later probe.
    pub fn transient(&self) -> bool {
        self.state == State::Unreachable && self.retry_after.is_none_or(|d| d <= Duration::from_secs(2))
    }
}

/// Classify an HTTP error answer (`status` not 2xx) from its status, body and `Retry-After`.
pub fn classify(status: u16, body: &str, retry_after: Option<Duration>) -> Failure {
    let (code, message) = api_error(body);
    let detail = message.clone().unwrap_or_else(|| format!("HTTP {status}"));
    let code = code.unwrap_or_default();
    let mut failure = match status {
        401 => Failure::new(State::KeyRejected, detail),
        403 => Failure::new(State::NoAccess, detail),
        404 if code == "model_not_found" || message.as_deref().is_some_and(|m| m.contains("model")) => {
            Failure::new(State::Incompatible, detail)
        }
        404 => Failure::new(State::NoAccess, detail),
        429 if code == "insufficient_quota" => Failure::new(State::NoCredit, detail),
        429 => Failure::new(State::Unreachable, format!("rate limited ({detail})")),
        400 | 422 => Failure::new(State::Incompatible, detail),
        _ => Failure::new(State::Unreachable, format!("HTTP {status}{}", message.map(|m| format!(": {m}")).unwrap_or_default())),
    };
    failure.retry_after = retry_after;
    failure
}

/// `{"error": {"code": ..., "message": ...}}`, the API's error shape.
fn api_error(body: &str) -> (Option<String>, Option<String>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else { return (None, None) };
    let e = v.get("error");
    let s = |k: &str| e.and_then(|e| e.get(k)).and_then(|v| v.as_str()).map(|m| m.chars().take(200).collect::<String>());
    (s("code").or_else(|| s("type")), s("message"))
}

/// The plugin's view of the decision model.
#[derive(Debug, Clone, PartialEq)]
pub struct Health {
    pub state: State,
    pub detail: Option<String>,
    /// The key is set but the Keychain wouldn't hand it over.
    pub unreadable: bool,
    pub last_attempt: Option<SystemTime>,
    pub next_attempt: Option<SystemTime>,
    malformed: u32,
}

impl Health {
    pub fn new(has_key: bool, unreadable: bool, now: SystemTime) -> Self {
        let mut h = Self { state: State::NotSet, detail: None, unreadable, last_attempt: None, next_attempt: None, malformed: 0 };
        h.key_changed(has_key, unreadable, now);
        h
    }

    /// The key was set, replaced or removed: check a new one at once.
    pub fn key_changed(&mut self, has_key: bool, unreadable: bool, now: SystemTime) {
        self.unreadable = unreadable;
        self.malformed = 0;
        self.detail = None;
        self.last_attempt = None;
        if has_key {
            self.state = State::Checking;
            self.next_attempt = Some(now);
        } else {
            self.state = State::NotSet;
            self.next_attempt = None;
        }
    }

    /// A key just validated: no need to probe it again.
    pub fn validated(&mut self, now: SystemTime) {
        self.success(now);
    }

    /// A review or probe succeeded. Returns whether this is a recovery (it wasn't healthy).
    pub fn success(&mut self, now: SystemTime) -> bool {
        let recovered = self.state != State::Healthy;
        self.state = State::Healthy;
        self.detail = None;
        self.last_attempt = Some(now);
        self.next_attempt = None;
        self.malformed = 0;
        recovered
    }

    /// A review or probe failed: note it and schedule the next probe (`interval` from now, or
    /// later if the server asked).
    pub fn failure(&mut self, f: &Failure, now: SystemTime, interval: Duration) {
        self.malformed = if f.malformed { self.malformed + 1 } else { 0 };
        self.state = if f.malformed && self.malformed >= MALFORMED_LIMIT { State::Incompatible } else { f.state };
        self.detail = Some(f.detail.clone());
        self.last_attempt = Some(now);
        self.next_attempt = Some(now + f.retry_after.map_or(interval, |r| r.max(interval)));
    }

    /// "Retry now", or the key changed: probe at once.
    pub fn retry_now(&mut self, now: SystemTime) {
        if self.state != State::NotSet && self.state != State::Healthy {
            self.next_attempt = Some(now);
        }
    }

    /// A probe is due at `now`.
    pub fn probe_due(&self, now: SystemTime) -> bool {
        self.state != State::Healthy && self.state != State::NotSet && self.next_attempt.is_some_and(|t| t <= now)
    }

    /// Workspace Write and Full access can be used.
    pub fn ready(&self) -> bool {
        self.state == State::Healthy
    }

    /// The blocker sentence for a mode named `mode` (also the reason of a held call).
    pub fn problem(&self, mode: &str) -> Option<String> {
        let detail = || self.detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default();
        Some(match self.state {
            State::Healthy => return None,
            State::NotSet if self.unreadable => {
                format!("{mode} needs your OpenAI API key, but Space couldn't read it from the Keychain.")
            }
            State::NotSet => format!("{mode} checks every action with OpenAI first. Add an OpenAI API key to use it."),
            State::Checking => "Checking your OpenAI API key…".to_string(),
            State::Unreachable => format!("Can't reach OpenAI{}, so actions are paused.", detail()),
            State::KeyRejected => "OpenAI rejected your API key, so actions are paused.".to_string(),
            State::NoAccess => {
                format!("This OpenAI account doesn't have the Decisions API{}, so actions are paused.", detail())
            }
            State::NoCredit => "OpenAI says the account is out of credit, so actions are paused.".to_string(),
            State::Incompatible => {
                format!("OpenAI's decision API changed{}; Space needs an update. Actions are paused.", detail())
            }
        })
    }
}

/// Unix milliseconds, for the blocker's times.
pub fn unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN5: Duration = Duration::from_secs(300);

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_000_000 + secs)
    }

    #[test]
    fn classifies_every_status() {
        let c = |status, body: &str| classify(status, body, None).state;
        assert_eq!(c(401, r#"{"error":{"code":"invalid_api_key","message":"Incorrect API key"}}"#), State::KeyRejected);
        assert_eq!(c(403, r#"{"error":{"message":"Decision API is not enabled for this user."}}"#), State::NoAccess);
        assert_eq!(c(404, r#"{"error":{"message":"Not found"}}"#), State::NoAccess);
        assert_eq!(c(404, r#"{"error":{"code":"model_not_found","message":"The model gpt-6-luna does not exist"}}"#), State::Incompatible);
        assert_eq!(c(429, r#"{"error":{"code":"insufficient_quota","message":"You exceeded your quota"}}"#), State::NoCredit);
        assert_eq!(c(429, r#"{"error":{"code":"rate_limit_exceeded","message":"slow down"}}"#), State::Unreachable);
        assert_eq!(c(400, r#"{"error":{"message":"Unknown parameter: questions[0].choices"}}"#), State::Incompatible);
        assert_eq!(c(503, "upstream overloaded"), State::Unreachable);
        assert_eq!(c(500, r#"{"error":{"message":"boom"}}"#), State::Unreachable);
        let f = classify(503, r#"{"error":{"message":"overloaded"}}"#, None);
        assert_eq!(f.detail, "HTTP 503: overloaded");
        assert!(f.transient());
        let limited = classify(429, "", Some(Duration::from_secs(600)));
        assert!(!limited.transient() && limited.retry_after == Some(Duration::from_secs(600)));
        assert!(!classify(401, "", None).transient());
    }

    #[test]
    fn schedule_follows_failures_and_recoveries() {
        let mut h = Health::new(true, false, at(0));
        assert_eq!(h.state, State::Checking);
        assert!(h.probe_due(at(0)) && !h.ready());

        // A failure: next probe in 5 minutes; the blocker says why.
        h.failure(&Failure::new(State::KeyRejected, "Incorrect API key"), at(1), MIN5);
        assert_eq!((h.last_attempt, h.next_attempt), (Some(at(1)), Some(at(301))));
        assert!(!h.probe_due(at(300)) && h.probe_due(at(301)));
        assert!(h.problem("Workspace Write").unwrap().starts_with("OpenAI rejected your API key"));
        // "Retry now" brings it forward.
        h.retry_now(at(10));
        assert!(h.probe_due(at(10)));

        // A Retry-After beyond the interval wins; a shorter one doesn't.
        let mut slow = Failure::new(State::Unreachable, "rate limited");
        slow.retry_after = Some(Duration::from_secs(900));
        h.failure(&slow, at(20), MIN5);
        assert_eq!(h.next_attempt, Some(at(920)));
        slow.retry_after = Some(Duration::from_secs(30));
        h.failure(&slow, at(20), MIN5);
        assert_eq!(h.next_attempt, Some(at(320)));

        // Recovery: healthy, nothing scheduled, reported as a recovery once.
        assert!(h.success(at(400)));
        assert!(h.ready() && h.problem("x").is_none() && !h.probe_due(at(10_000)));
        assert!(!h.success(at(401)));
    }

    #[test]
    fn malformed_answers_turn_into_incompatible() {
        let mut h = Health::new(true, false, at(0));
        let mut bad = Failure::new(State::Unreachable, "unreadable answer");
        bad.malformed = true;
        h.failure(&bad, at(1), MIN5);
        h.failure(&bad, at(2), MIN5);
        assert_eq!(h.state, State::Unreachable);
        h.failure(&bad, at(3), MIN5);
        assert_eq!(h.state, State::Incompatible);
        // Another kind of failure resets the streak.
        h.failure(&Failure::new(State::Unreachable, "timed out"), at(4), MIN5);
        h.failure(&bad, at(5), MIN5);
        assert_eq!(h.state, State::Unreachable);
    }

    #[test]
    fn key_changes() {
        let mut h = Health::new(false, false, at(0));
        assert_eq!(h.state, State::NotSet);
        assert!(!h.probe_due(at(0)));
        assert!(h.problem("Workspace Write").unwrap().contains("Add an OpenAI API key"));
        h.key_changed(true, false, at(5));
        assert!(h.probe_due(at(5)));
        h.validated(at(6));
        assert!(h.ready());
        h.key_changed(false, true, at(7));
        assert!(h.problem("Full access").unwrap().contains("Keychain"));
    }
}
