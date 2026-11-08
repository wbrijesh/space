//! Threads: their state, applying session events, sessions and the message queue (a message
//! sent before its session exists waits on the thread). Saving and reopening them is in
//! [`crate::persist`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use gpui::*;
use space_core::protocol::ConfigureSession;
use space_core::protocol::service::ThreadMeta;
use space_core::{AgentHandle, PluginStatus, Session, SessionEvent, SessionState, TranscriptItem};
use space_ui::format;

use crate::app::{SeedThread, SpaceApp};
use crate::attach::{DraftImage, thread_attachments_dir};
use crate::images::Prepared;
use crate::settings::ModelRef;

/// One conversation with the agent (an ACP session once it has been created).
pub struct Thread {
    pub key: u64,
    /// Stable id: the saved thread's folder name.
    pub id: String,
    pub created_ms: u64,
    /// The summary last saved (or read from disk); `None` until the thread is first saved.
    pub meta: Option<ThreadMeta>,
    /// Events to save next (agent text and thoughts merged).
    pub unsaved: Vec<SessionEvent>,
    /// How many of its events the store has.
    pub saved: u64,
    /// A save is in flight, with these events.
    pub saving: bool,
    pub inflight: Vec<SessionEvent>,
    /// A save was asked for while one was in flight (or the thread was loading).
    pub save_again: bool,
    /// A saved thread whose events haven't been read yet (they are when it's opened).
    pub unloaded: bool,
    pub loading: bool,
    /// The title the `session-title` plugin gave it; until then (or without one) the first line
    /// of the first message.
    pub name: Option<String>,
    /// The agent session to load to continue a saved conversation: (agent plugin, session id).
    pub resume: Option<(String, String)>,
    pub cwd: PathBuf,
    pub branch: Option<String>,
    pub session: Option<Session>,
    pub state: SessionState,
    /// `session/new` is in flight.
    pub creating: bool,
    /// Creating the session failed.
    pub error: Option<String>,
    /// Turns that finished (or need input) while the thread was not selected.
    pub unread: usize,
    pub draft: String,
    pub attachments: Vec<PathBuf>,
    /// ↑/↓ through the sent messages.
    pub recall: crate::recall::Recall,
    /// Images in the draft, in order.
    pub images: Vec<DraftImage>,
    /// Where the thread's sent images are kept (from the session store; known once a session
    /// was created).
    pub attachments_dir: Option<PathBuf>,
    /// Message sent before the session existed; sent once it does.
    pub queued: Option<Queued>,
    /// Permission mode picked for this thread (a `permission-modes` id); `None`: the default
    /// for new threads. Applied to every session the thread gets.
    pub mode: Option<String>,
    /// Modes that need confirming (Full access) and were confirmed in this thread.
    pub confirmed: HashSet<String>,
    /// This turn already counted a paused tool call as unread.
    paused_seen: bool,
    /// Expanded thought/tool items (transcript indices).
    pub expanded: HashSet<usize>,
    /// Finished turns opened to show their work (by the index of their user message); in
    /// memory only.
    pub open_turns: HashSet<usize>,
    thought_started: HashMap<usize, Instant>,
    pub thought_secs: HashMap<usize, u64>,
}

/// A message waiting for the session to start.
pub struct Queued {
    pub text: String,
    pub attachments: Vec<PathBuf>,
    pub images: Vec<Prepared>,
}

impl Thread {
    pub(crate) fn new(key: u64, cwd: PathBuf) -> Self {
        Self {
            key,
            id: crate::persist::new_thread_id(),
            created_ms: crate::app::now_ms(),
            meta: None,
            unsaved: Vec::new(),
            saved: 0,
            saving: false,
            inflight: Vec::new(),
            save_again: false,
            unloaded: false,
            loading: false,
            name: None,
            resume: None,
            branch: format::git_branch(&cwd),
            cwd,
            session: None,
            state: SessionState::default(),
            creating: false,
            error: None,
            unread: 0,
            draft: String::new(),
            attachments: Vec::new(),
            recall: Default::default(),
            images: Vec::new(),
            attachments_dir: None,
            queued: None,
            mode: None,
            confirmed: HashSet::new(),
            paused_seen: false,
            expanded: HashSet::new(),
            open_turns: HashSet::new(),
            thought_started: HashMap::new(),
            thought_secs: HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.state.items.is_empty() && self.queued.is_none() && self.meta.is_none()
    }

    pub fn title(&self) -> String {
        self.name
            .clone()
            .or_else(|| format::thread_title(&self.state))
            .or_else(|| self.meta.as_ref().map(|m| m.title.clone()))
            .or_else(|| self.queued.as_ref().map(|q| format::truncate_chars(&format::one_line(&q.text), 120)))
            .unwrap_or_else(|| "New thread".into())
    }

    /// The session can take a prompt now.
    pub fn live_session(&self) -> Option<&Session> {
        self.session.as_ref().filter(|s| s.agent().is_running())
    }

    pub(crate) fn apply(&mut self, event: &SessionEvent) {
        self.state.apply(event);
        let now = Instant::now();
        let last = self.state.items.len().checked_sub(1);
        match (last, self.state.items.last()) {
            (Some(ix), Some(TranscriptItem::Thought(_))) => {
                self.thought_started.entry(ix).or_insert(now);
            }
            _ => {
                for (ix, start) in &self.thought_started {
                    self.thought_secs.entry(*ix).or_insert_with(|| now.duration_since(*start).as_secs());
                }
            }
        }
        if matches!(event, SessionEvent::TurnEnded { .. }) {
            self.branch = format::git_branch(&self.cwd);
        }
    }
}

impl SpaceApp {
    pub(crate) fn apply_session_event(&mut self, key: u64, event: &SessionEvent, cx: &mut Context<Self>) {
        if let SessionEvent::Status(status) = event {
            // What the agent offers, for the Settings page.
            if let Some(models) = &status.available_models {
                self.agent_models = Some(models.clone());
            }
            if let Some(levels) = &status.thinking_levels {
                self.agent_levels = Some(levels.clone());
            }
        }
        // On the Settings page nothing is being watched.
        let selected = self.selected == Some(key) && !self.show_settings;
        let Some(thread) = self.thread_mut(key) else { return };
        thread.apply(event);
        if selected && matches!(event, SessionEvent::TurnEnded { .. }) && !self.transcript_scroll.is_following() {
            let thread = self.thread_mut(key).expect("thread exists");
            if let Some(user) = thread.state.items.iter().rposition(|i| matches!(i, TranscriptItem::User { .. })) {
                thread.open_turns.insert(user);
            }
        }
        if self.saving_threads() {
            let thread = self.thread_mut(key).expect("thread exists");
            crate::persist::record(&mut thread.unsaved, event);
            if matches!(event, SessionEvent::TurnEnded { .. }) {
                self.save_thread(key, cx);
            }
        }
        let Some(thread) = self.thread_mut(key) else { return };
        // A tool call waiting for the guardrail counts once per turn (it may stay paused long).
        let paused = match event {
            SessionEvent::TurnStarted => {
                thread.paused_seen = false;
                false
            }
            SessionEvent::ToolCallHeld { reason: Some(_), .. } if !thread.paused_seen => {
                thread.paused_seen = true;
                true
            }
            _ => false,
        };
        let attention = paused
            || matches!(
                event,
                SessionEvent::TurnEnded { .. } | SessionEvent::PermissionRequested(_) | SessionEvent::Error { .. }
            );
        if !selected && attention && !matches!(event, SessionEvent::TurnEnded { stop_reason, .. } if stop_reason == "error") {
            thread.unread += 1;
        }
    }

    pub(crate) fn add_thread(&mut self, cwd: PathBuf) -> u64 {
        let key = self.next_key;
        self.next_key += 1;
        self.threads.push(Thread::new(key, cwd));
        key
    }

    /// Start the agent plugin if it is not running; sessions are created once it is up.
    pub(crate) fn ensure_agent(&mut self, cx: &mut Context<Self>) {
        if self.agent_running() || self.agent_status == PluginStatus::Starting {
            return;
        }
        let Some(id) = self.agent_id.clone() else { return };
        self.agent_status = PluginStatus::Starting;
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            let result = host.start_agent(&id).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(agent) => {
                        this.agent = Some(agent);
                        this.agent_status = PluginStatus::Running;
                        let woke = std::mem::take(&mut this.agent_asleep);
                        this.touch_agent();
                        this.create_pending_sessions(cx);
                        // Woken by writing: load the thread you're in now, not when you send.
                        if woke
                            && let Some(key) = this.selected
                            && this.thread(key).is_some_and(|t| t.session.is_none() && !t.creating && t.resume.is_some())
                        {
                            this.create_session(key, cx);
                        }
                    }
                    Err(e) => {
                        // The host's own status says whether it's waiting for something
                        // (`NotStarted`: it starts once that runs) or failed.
                        this.agent_status = this
                            .host
                            .status(&id)
                            .filter(|s| matches!(s, PluginStatus::NotStarted(_)))
                            .unwrap_or_else(|| PluginStatus::Crashed(format!("{e:#}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn create_pending_sessions(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<u64> = self
            .threads
            .iter()
            // A saved conversation gets its session when you write in it.
            .filter(|t| t.session.is_none() && !t.creating && t.error.is_none() && t.meta.is_none())
            .map(|t| t.key)
            .collect();
        for key in keys {
            self.create_session(key, cx);
        }
    }

    pub(crate) fn create_session(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(agent) = self.agent.clone().filter(AgentHandle::is_running) else {
            self.ensure_agent(cx);
            return;
        };
        let Some(thread) = self.thread(key) else { return };
        let defaults = self.session_defaults(thread);
        let host = self.host.clone();
        // Sessions are kept (and can be loaded) only with a session store.
        let can_load = agent.can_load_session() && self.saving_threads();
        let Some(thread) = self.thread_mut(key) else { return };
        thread.creating = true;
        thread.error = None;
        // Continue the saved conversation if this agent can; else start over in a new session.
        let resume = thread.resume.take().filter(|(plugin, _)| plugin == agent.plugin_id() && can_load);
        if resume.is_none() && !thread.state.items.is_empty() {
            thread.state.items.push(TranscriptItem::divider("new-session"));
        }
        let (cwd, id) = (thread.cwd.clone(), thread.id.clone());
        cx.spawn(async move |this, cx| {
            let dirs = host.thread_dirs(&id).await;
            let _ = this.update(cx, |this, _| {
                if let Some(t) = this.thread_mut(key) {
                    t.attachments_dir = Some(dirs.attachments.clone());
                }
            });
            let data_dir = dirs.data;
            let result = match &resume {
                Some((_, session_id)) => agent.load_session(session_id, cwd, data_dir, Some(&id)).await,
                // An agent that can't use the folder still gets a session (kept in memory).
                None => match agent.new_session_in(cwd.clone(), data_dir.clone(), Some(&id)).await {
                    Err(e) if data_dir.is_some() => {
                        tracing::warn!("a session in {}: {e:#}; keeping it in memory", data_dir.unwrap_or_default().display());
                        agent.new_session_in(cwd, None, Some(&id)).await
                    }
                    result => result,
                },
            };
            let session = match result {
                Ok(s) => s,
                Err(e) => {
                    let _ = this.update(cx, |this, cx| {
                        let Some(t) = this.thread_mut(key) else { return };
                        t.creating = false;
                        if resume.is_some() {
                            // The saved session is gone: go on in a new one.
                            tracing::warn!("loading the saved session failed: {e:#}");
                            this.create_session(key, cx);
                        } else {
                            t.error = Some(format!("{e:#}"));
                        }
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                // Guards judge a loaded session's calls with what the thread already shows.
                if resume.is_some()
                    && let Some(t) = this.thread(key)
                {
                    session.seed_history(&t.state.items);
                }
                this.attach_session(key, session.clone(), cx);
            });
            if let Some(request) = defaults {
                apply_defaults(&session, request).await;
            }
            let _ = this.update(cx, |this, cx| {
                if let Some(t) = this.thread_mut(key) {
                    t.creating = false;
                    if let Some(q) = t.queued.take() {
                        this.prompt(key, q.text, q.attachments, q.images, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// What a new session should be switched to: for a new thread, the defaults from
    /// Settings; for a thread whose agent exited, the model and level it was using.
    pub(crate) fn session_defaults(&self, thread: &Thread) -> Option<ConfigureSession> {
        let request = if thread.state.items.is_empty() {
            let d = &self.settings.settings.new_threads;
            let (provider, model) =
                d.model.clone().map_or((None, None), |ModelRef { provider, id }| (Some(provider), Some(id)));
            ConfigureSession { provider, model, thinking_level: d.thinking_level.clone(), ..Default::default() }
        } else {
            let s = &thread.state.status;
            ConfigureSession {
                provider: s.provider.clone(),
                model: s.model.clone(),
                thinking_level: s.thinking_level.clone(),
                ..Default::default()
            }
        };
        (request.model.is_some() || request.thinking_level.is_some()).then_some(request)
    }

    pub(crate) fn attach_session(&mut self, key: u64, session: Session, cx: &mut Context<Self>) {
        let sid = session.id().to_string();
        let Some(thread) = self.thread_mut(key) else { return };
        if thread.state.id.is_empty() {
            thread.state.id.clone_from(&sid);
        }
        thread.state.status.session_id.clone_from(&sid);
        thread.state.cwd.clone_from(&thread.cwd);
        thread.state.plugin_id = session.agent().plugin_id().to_string();
        thread.session = Some(session);
        self.session_threads.insert(sid.clone(), key);
        self.sync_session_modes();
        for event in self.orphans.remove(&sid).unwrap_or_default() {
            self.apply_session_event(key, &event, cx);
        }
        cx.notify();
    }

    /// Open the launch threads (one empty thread when `seed` is empty). Prompts are queued
    /// and sent once the agent is up.
    pub(crate) fn seed_threads(&mut self, seed: Vec<SeedThread>, select: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let seed = if seed.is_empty() { vec![SeedThread::default()] } else { seed };
        let mut keys = Vec::new();
        let mut scrolled_up = Vec::new();
        for s in seed {
            let key = self.add_thread(s.cwd.unwrap_or_else(|| self.project_dir.clone()));
            let prepare = |paths: Vec<PathBuf>| -> Vec<Prepared> {
                paths.iter().filter_map(|p| crate::images::prepare_file(p).map_err(|e| tracing::warn!("{}: {e}", p.display())).ok()).collect()
            };
            let (images, drafts) = (prepare(s.images), prepare(s.draft_images));
            let first_id = self.next_image_id;
            self.next_image_id += drafts.len() as u64;
            if let Some(t) = self.thread_mut(key) {
                t.queued = s.prompt.map(|text| Queued { text, attachments: Vec::new(), images });
                t.images = drafts.into_iter().zip(first_id..).map(|(p, id)| DraftImage::ready(id, p)).collect();
                t.draft = s.draft.unwrap_or_default();
                t.mode = s.mode;
            }
            keys.push(key);
            scrolled_up.push(s.scrolled_up);
        }
        let ix = select.filter(|i| *i < keys.len()).unwrap_or(keys.len() - 1);
        let key = keys.get(ix).copied();
        self.selected = key;
        if let Some(distance) = scrolled_up[ix] {
            self.transcript_scroll.scroll_up_from_bottom(px(distance));
        }
        if let Some(draft) = key.and_then(|k| self.thread(k)).map(|t| t.draft.clone()).filter(|d| !d.is_empty()) {
            self.input.update(cx, |s, cx| s.set_value(draft, window, cx));
        }
    }

    pub(crate) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        self.touch_agent();
        let text = self.input.read(cx).value().trim().to_string();
        let Some(thread) = self.thread_mut(key) else { return };
        if text.is_empty() && thread.attachments.is_empty() && thread.images.is_empty() {
            return;
        }
        // Images still being prepared: the send button waits for them.
        if thread.images.iter().any(|i| i.prepared.is_none()) {
            return;
        }
        // Not into a saved thread that isn't open (it didn't load).
        if thread.state.running || thread.queued.is_some() || thread.loading || thread.unloaded {
            return;
        }
        // The thread's mode isn't set up yet (the composer says so, with a link).
        if let Some(t) = self.thread(key)
            && self.blocker(t).is_some()
        {
            return;
        }
        let Some(thread) = self.thread_mut(key) else { return };
        let attachments = std::mem::take(&mut thread.attachments);
        thread.recall.end();
        let images: Vec<Prepared> = std::mem::take(&mut thread.images).into_iter().filter_map(|i| i.prepared).collect();
        thread.draft.clear();
        self.input.update(cx, |s, cx| s.clean(window, cx));

        let thread = self.thread_mut(key).expect("thread exists");
        if thread.live_session().is_some() {
            self.prompt(key, text, attachments, images, cx);
        } else {
            // No session yet (agent starting) or its agent exited: send once there is one (the
            // old session is loaded again if the agent can).
            if let Some(old) = thread.session.take() {
                thread.resume = Some((old.agent().plugin_id().to_string(), old.id().to_string()));
            }
            thread.queued = Some(Queued { text, attachments, images });
            thread.error = None;
            if !thread.creating {
                self.create_session(key, cx);
            }
        }
        self.transcript_scroll.jump_to_bottom();
        cx.notify();
    }

    /// Send a message: its images are saved in the thread's attachments folder first (an image
    /// that can't be saved stops the send and says why), then the prompt goes out.
    pub(crate) fn prompt(&mut self, key: u64, text: String, attachments: Vec<PathBuf>, images: Vec<Prepared>, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        let Some(session) = thread.session.clone() else { return };
        // A thread that was saved already had its first message.
        let first = thread.name.is_none()
            && thread.meta.is_none()
            && !thread.state.items.iter().any(|i| matches!(i, TranscriptItem::User { .. }));
        if first && !text.trim().is_empty() {
            self.request_title(key, session.id().to_string(), text.clone(), cx);
        }
        let dir = thread_attachments_dir(self.thread(key));
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_spawn(async move { images.iter().map(|i| crate::images::save(i, &dir)).collect::<std::io::Result<Vec<_>>>() })
                .await;
            let images = match saved {
                Ok(images) => images,
                Err(e) => {
                    let _ = this.update(cx, |this, cx| {
                        this.banner = Some(format!("Couldn't save an image for this message: {e}"));
                        cx.notify();
                    });
                    return;
                }
            };
            // Failures are reported as session events (Error + TurnEnded).
            let _ = session.prompt_with(text, attachments, images).await;
        })
        .detach();
    }

    /// Ask the `session-title` plugin to name the thread after its first message; it keeps the
    /// first-line title until a title arrives, and if none does.
    fn request_title(&mut self, key: u64, session_id: String, first_message: String, cx: &mut Context<Self>) {
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            let Some(title) = host.generate_title(&session_id, &first_message, space_core::TITLE_TIMEOUT).await else { return };
            let _ = this.update(cx, |this, cx| {
                let Some(thread) = this.thread_mut(key) else { return };
                thread.name = Some(title);
                if this.saving_threads() {
                    this.save_thread(key, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// Switch a just-created session to `request`. When that fails (say the model is no longer
/// offered), keep the agent's model but still try the thinking level; otherwise stay on the
/// agent's defaults.
async fn apply_defaults(session: &Session, request: ConfigureSession) {
    let level = request.thinking_level.clone().filter(|_| request.model.is_some());
    if let Err(e) = session.configure(request).await {
        tracing::info!("new thread keeps the agent's model: {e:#}");
        if let Some(level) = level {
            let request = ConfigureSession { thinking_level: Some(level), ..Default::default() };
            if let Err(e) = session.configure(request).await {
                tracing::info!("new thread keeps the agent's thinking level: {e:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use space_core::SessionEvent;

    use super::Thread;

    #[test]
    fn a_generated_title_replaces_the_first_line() {
        let mut thread = Thread::new(1, PathBuf::from("/nonexistent"));
        assert_eq!(thread.title(), "New thread");
        thread.apply(&SessionEvent::UserMessage { text: "please fix the login page\nit redirects".into(), attachments: vec![], images: vec![], at_ms: None });
        // Until a title arrives (or if none does): the first line.
        assert_eq!(thread.title(), "please fix the login page");
        thread.name = Some("Fix login redirect".into());
        assert_eq!(thread.title(), "Fix login redirect");
    }
}
