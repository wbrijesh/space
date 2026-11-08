//! ↑/↓ in the composer: step back through the messages sent in the thread, and forward again to
//! the draft that was there before.

use gpui::component::input::RopeExt as _;
use gpui::*;
use space_core::TranscriptItem;
use space_ui::components::composer::RecallStep;

use crate::app::SpaceApp;

/// Where a thread's composer is in its sent messages. Not recalling until the first ↑; editing
/// the text ends it (the recalled text is the draft from then on).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Recall {
    /// The index into the history shown now (`None`: not recalling).
    pos: Option<usize>,
    /// The composer's text before the first ↑, given back after the newest message.
    draft: String,
}

impl Recall {
    /// ↑: the message before the one shown (the newest, on the first press), or `None` when
    /// there's nothing older (the key then moves the caret as usual). `history` is oldest first;
    /// `current` is the composer's text (kept as the draft on the first press).
    pub fn back(&mut self, history: &[String], current: &str) -> Option<String> {
        let pos = match self.pos {
            None if history.is_empty() => return None,
            None => {
                self.draft = current.to_string();
                history.len() - 1
            }
            Some(0) => return None,
            Some(pos) => pos - 1,
        };
        self.pos = Some(pos);
        history.get(pos).cloned()
    }

    /// ↓: the message after the one shown, then the draft; `None` when not recalling.
    pub fn forward(&mut self, history: &[String]) -> Option<String> {
        let pos = self.pos?;
        if pos + 1 < history.len() {
            self.pos = Some(pos + 1);
            return history.get(pos + 1).cloned();
        }
        self.pos = None;
        Some(std::mem::take(&mut self.draft))
    }

    /// The text was edited (or sent): stop recalling.
    pub fn end(&mut self) {
        self.pos = None;
        self.draft.clear();
    }

    #[cfg(test)]
    pub fn active(&self) -> bool {
        self.pos.is_some()
    }
}

/// The sent messages a thread's composer steps through, oldest first: their text, trimmed,
/// without empty ones (image-only messages) and without a message repeating the one before.
pub fn history<'a>(sent: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for text in sent.into_iter().map(str::trim).filter(|t| !t.is_empty()) {
        if out.last().map(String::as_str) != Some(text) {
            out.push(text.to_string());
        }
    }
    out
}

// ----- the composer's ↑/↓ ------------------------------------------------------------------


impl SpaceApp {
    /// ↑/↓ at the edge of the composer's text: put the previous / next sent message of the
    /// thread there (caret at the end). `false`: nothing to recall, the key moves the caret.
    pub(crate) fn recall(&mut self, step: RecallStep, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(key) = self.selected else { return false };
        let current = self.input.read(cx).value().to_string();
        let Some(thread) = self.thread_mut(key) else { return false };
        let sent = history(thread.state.items.iter().filter_map(|i| match i {
            TranscriptItem::User { text, .. } => Some(text.as_str()),
            _ => None,
        }));
        let text = match step {
            RecallStep::Back => thread.recall.back(&sent, &current),
            RecallStep::Forward => thread.recall.forward(&sent),
        };
        let Some(text) = text else { return false };
        // `set_value` doesn't report a change, so this doesn't count as editing.
        self.input.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            let end = s.text().offset_to_position(s.text().len());
            s.set_cursor_position(end, window, cx);
        });
        cx.notify();
        true
    }

    /// The composer's text was edited (or sent): the thread stops recalling.
    pub(crate) fn end_recall(&mut self) {
        if let Some(key) = self.selected
            && let Some(thread) = self.thread_mut(key)
        {
            thread.recall.end();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Recall, history};

    fn sent() -> Vec<String> {
        history(["fix the test", "", "run it again", "run it again", "  ship it  "])
    }

    #[test]
    fn history_skips_empty_and_repeated_messages() {
        assert_eq!(sent(), ["fix the test", "run it again", "ship it"]);
    }

    #[test]
    fn up_goes_back_and_down_returns_to_the_draft() {
        let h = sent();
        let mut r = Recall::default();
        assert_eq!(r.back(&h, "half a tho").as_deref(), Some("ship it"));
        assert_eq!(r.back(&h, "ship it").as_deref(), Some("run it again"));
        assert_eq!(r.back(&h, "run it again").as_deref(), Some("fix the test"));
        // Nothing older: the key moves the caret instead.
        assert_eq!(r.back(&h, "fix the test"), None);
        assert!(r.active());
        assert_eq!(r.forward(&h).as_deref(), Some("run it again"));
        assert_eq!(r.forward(&h).as_deref(), Some("ship it"));
        assert_eq!(r.forward(&h).as_deref(), Some("half a tho"));
        assert!(!r.active());
        // Not recalling: ↓ is the caret's again.
        assert_eq!(r.forward(&h), None);
    }

    #[test]
    fn editing_ends_recall_and_empty_history_does_nothing() {
        let h = sent();
        let mut r = Recall::default();
        r.back(&h, "draft");
        r.end();
        assert!(!r.active());
        assert_eq!(r.forward(&h), None);
        // The next ↑ starts over from the newest, keeping what's there now as the draft.
        assert_eq!(r.back(&h, "edited").as_deref(), Some("ship it"));
        assert_eq!(r.forward(&h).as_deref(), Some("edited"));

        let mut r = Recall::default();
        assert_eq!(r.back(&[], "anything"), None);
        assert!(!r.active());
    }
}
