//! Messages typed while the agent works.
//!
//! ⏎ queues a message for when the turn is done: Space keeps it and sends it once the turn ends
//! normally. Each queued message is its own turn, oldest first. ⌘⏎ sends it after the current step: it
//! goes to the agent at once (`Session::steer`), which hands it to the model before its next
//! request; it joins the transcript then. An agent that can't steer gets it when done instead.
//!
//! Queued messages wait above the composer, where they can be taken back to edit or dropped.
//! Stopping the turn sends none of them: they go back into the composer. A turn that fails keeps
//! them, with a button to send them now.

use gpui::*;
use space_core::SessionId;

use crate::app::SpaceApp;
use crate::attach::DraftImage;
use crate::threads::Outgoing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum When {
    /// When the turn is done.
    Done,
    /// After the current step (steered into the turn).
    Step,
}

#[derive(Clone)]
pub(crate) struct Pending {
    pub(crate) id: String,
    pub(crate) message: Outgoing,
    pub(crate) when: When,
}

/// What a queued message's row asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingAction {
    /// Back into the composer.
    Edit,
    Remove,
    /// Send what's queued now (the turn isn't running).
    SendNow,
}

impl SpaceApp {
    /// Queue a message for a running turn.
    pub(crate) fn queue_message(&mut self, key: u64, when: When, message: Outgoing, cx: &mut Context<Self>) {
        self.next_pending += 1;
        let id = format!("queued-{}", self.next_pending);
        let can_steer = self.thread(key).and_then(|t| t.session.as_ref()).is_some_and(|s| s.agent().can_steer());
        let when = if when == When::Step && can_steer { When::Step } else { When::Done };
        let Some(thread) = self.thread_mut(key) else { return };
        thread.pending.push(Pending { id: id.clone(), message, when });
        if when == When::Step {
            self.steer(key, &id, cx);
        }
        cx.notify();
    }

    /// Hand a queued message to the agent for its next step. If it can't take it (the turn just
    /// ended), it waits for when the turn is done instead.
    fn steer(&mut self, key: u64, id: &str, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        let (Some(session), Some(p)) = (thread.session.clone(), thread.pending.iter().find(|p| p.id == id).cloned()) else { return };
        let dir = crate::attach::thread_attachments_dir(Some(thread));
        cx.spawn(async move |this, cx| {
            let (images, pastes) = (p.message.images.clone(), p.message.pastes.clone());
            let saved = cx.background_spawn(async move { crate::paste::save_all(&images, &pastes, &dir) }).await;
            let result = match saved {
                Ok((images, texts)) => session.steer(p.id.clone(), p.message.input(images, texts)).await,
                Err(e) => Err(anyhow::anyhow!("couldn't save an attachment: {e}")),
            };
            if let Err(e) = result {
                tracing::info!("steer {}: {e:#}; sending it when the turn is done", p.id);
                let _ = this.update(cx, |this, cx| {
                    let delivered = {
                        let Some(t) = this.thread_mut(key) else { return };
                        match t.pending.iter_mut().find(|q| q.id == p.id) {
                            Some(q) => {
                                q.when = When::Done;
                                false
                            }
                            None => true,
                        }
                    };
                    // The turn ended while it was on its way: send it now.
                    if !delivered && this.thread(key).is_some_and(|t| !t.state.running) {
                        this.flush_pending(key, cx);
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// The agent handed a steered message to the model (it's in the transcript now).
    pub(crate) fn steered(&mut self, session_id: &SessionId, id: &str, cx: &mut Context<Self>) {
        let Some(&key) = self.session_threads.get(session_id) else { return };
        if let Some(t) = self.thread_mut(key) {
            t.pending.retain(|p| p.id != id);
        }
        cx.notify();
    }

    /// A turn ended: send what's queued (it ended normally), hand it back (it was stopped), or
    /// keep it (it failed).
    pub(crate) fn turn_ended_with_queue(&mut self, key: u64, stop_reason: &str, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        if thread.pending.is_empty() {
            return;
        }
        match stop_reason {
            "cancelled" => {
                self.unsteer(key, cx);
                if let Some(t) = self.thread_mut(key) {
                    t.restore_pending = true;
                }
            }
            "error" => {
                // Steered ones the agent no longer has: they wait like the rest.
                if let Some(t) = self.thread_mut(key) {
                    for p in &mut t.pending {
                        p.when = When::Done;
                    }
                }
            }
            _ => self.flush_pending(key, cx),
        }
        cx.notify();
    }

    /// Send the oldest queued message as its own turn (a steered one the agent didn't get counts
    /// too). The rest follow one by one, each when the turn before it ends.
    pub(crate) fn flush_pending(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_mut(key) else { return };
        if thread.pending.is_empty() || thread.state.running || thread.live_session().is_none() {
            return;
        }
        let p = thread.pending.remove(0);
        let mut message = p.message;
        message.text = message.text.trim().to_string();
        self.prompt(key, message, cx);
        if self.selected == Some(key) {
            self.transcript_scroll.jump_to_bottom();
        }
    }

    /// Take back the steered messages the agent hasn't delivered.
    fn unsteer(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(session) = self.thread(key).and_then(|t| t.session.clone()) else { return };
        if !self.thread(key).is_some_and(|t| t.pending.iter().any(|p| p.when == When::Step)) {
            return;
        }
        cx.background_spawn(async move {
            if let Err(e) = session.unsteer().await {
                tracing::info!("unsteer: {e:#}");
            }
        })
        .detach();
    }

    /// A queued message's row: edit (back into the composer), remove, or send now.
    pub(crate) fn pending_action(&mut self, ix: usize, action: PendingAction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        if action == PendingAction::SendNow {
            self.flush_pending(key, cx);
            cx.notify();
            return;
        }
        let Some(thread) = self.thread_mut(key) else { return };
        if ix >= thread.pending.len() {
            return;
        }
        let p = thread.pending.remove(ix);
        // The agent can only drop all steered messages at once: take them back, then steer the
        // ones that stay again.
        if p.when == When::Step {
            let again: Vec<String> = thread.pending.iter().filter(|q| q.when == When::Step).map(|q| q.id.clone()).collect();
            self.unsteer_then(key, again, cx);
        }
        if action == PendingAction::Edit {
            self.restore_to_composer(key, vec![p], window, cx);
        }
        cx.notify();
    }

    fn unsteer_then(&mut self, key: u64, again: Vec<String>, cx: &mut Context<Self>) {
        let Some(session) = self.thread(key).and_then(|t| t.session.clone()) else { return };
        cx.spawn(async move |this, cx| {
            let _ = session.unsteer().await;
            let _ = this.update(cx, |this, cx| {
                for id in again {
                    this.steer(key, &id, cx);
                }
            });
        })
        .detach();
    }

    /// Put messages back into the composer (after what's typed there), with their files,
    /// images and pastes.
    pub(crate) fn restore_to_composer(&mut self, key: u64, messages: Vec<Pending>, window: &mut Window, cx: &mut Context<Self>) {
        if messages.is_empty() {
            return;
        }
        let typed = self.input.read(cx).value().trim().to_string();
        let mut parts: Vec<String> = messages.iter().map(|p| p.message.text.trim().to_string()).filter(|t| !t.is_empty()).collect();
        if !typed.is_empty() {
            parts.push(typed);
        }
        let text = parts.join("\n\n");
        let mut ids = Vec::new();
        for _ in messages.iter().flat_map(|p| &p.message.images) {
            self.next_image_id += 1;
            ids.push(self.next_image_id);
        }
        let Some(thread) = self.thread_mut(key) else { return };
        let mut ids = ids.into_iter();
        for p in messages {
            let p = p.message;
            thread.attachments.extend(p.attachments);
            thread.pastes.extend(p.pastes);
            // One quote at a time: the latest one taken back stays.
            if p.quote.is_some() {
                thread.quote = p.quote;
            }
            for image in p.images {
                thread.images.push(DraftImage::ready(ids.next().unwrap_or_default(), image));
            }
        }
        self.input.update(cx, |s, cx| s.set_value(text, window, cx));
        self.focus_composer(window, cx);
    }

    /// After a stop, a thread's queued messages go back into its composer (done when it's
    /// drawn, which has the window).
    pub(crate) fn restore_stopped_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let Some(thread) = self.thread_mut(key).filter(|t| t.restore_pending && !t.state.running) else { return };
        thread.restore_pending = false;
        let pending = std::mem::take(&mut thread.pending);
        self.restore_to_composer(key, pending, window, cx);
    }
}
