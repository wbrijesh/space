//! Find in thread (⌘F).
//!
//! The bar sits at the top right of the transcript. It searches your messages and the agent's
//! replies as they're displayed (a reply's `**bold**` is found as "bold"), ignoring case; not
//! thoughts, tool calls or their output. Every match is highlighted and the current one more
//! strongly; ⏎ or ⌘G goes to the next, ⇧⏎ or ⇧⌘G to the previous (wrapping), and Esc closes it.
//! Going to a match in a long message that's cut short opens it. The matches follow a reply
//! while it streams in, without moving off the current one. Switching threads closes the bar.
//!
//! Replies are drawn from text views the app keeps ([`Replies`]), one per reply of the shown
//! thread, so their displayed text can be searched and highlighted (the markdown engine's range
//! highlights, which change no layout). Your messages are highlighted by their bubbles.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use gpui::component::input::{InputEvent, InputState};
use gpui::component::text::{RangeHighlight, RenderedText, TextViewState};
use gpui::*;
use space_core::TranscriptItem;
use space_ui::components::find::find_matches;
use space_ui::theme::color;

use crate::app::SpaceApp;

/// The text views of the shown thread's replies, by item index.
#[derive(Default)]
pub(crate) struct Replies {
    key: Option<u64>,
    pub(crate) states: HashMap<usize, Entity<TextViewState>>,
    /// What each was last given: the reply's length and how much of it was shown.
    synced: HashMap<usize, (usize, usize)>,
    /// The ones with find highlights.
    marked: HashSet<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Match {
    /// The transcript item.
    pub(crate) ix: usize,
    /// Bytes of its displayed text: a message's trimmed text, a reply's rendered text.
    pub(crate) range: Range<usize>,
}

pub(crate) struct Find {
    pub(crate) input: Entity<InputState>,
    pub(crate) open: bool,
    pub(crate) matches: Vec<Match>,
    pub(crate) current: Option<usize>,
    /// Matches in messages, by item index (what their bubbles highlight).
    pub(crate) user: HashMap<usize, Vec<Range<usize>>>,
    /// Asks the current match's bubble to scroll to it once it's laid out.
    pub(crate) reveal: Rc<Cell<bool>>,
    /// What the matches were found in: the query, the thread, its item count and its replies'
    /// texts. Searched again when any of it changes.
    seen: Option<(String, u64, usize, Vec<RenderedText>)>,
    /// Go to a match after the next search (the query changed, or the bar opened).
    jump: bool,
    _sub: Subscription,
}

impl Find {
    pub(crate) fn current_match(&self) -> Option<&Match> {
        self.current.and_then(|i| self.matches.get(i))
    }

    /// "n of m" for the bar: `None` while the field is empty.
    pub(crate) fn count(&self, cx: &App) -> Option<(Option<usize>, usize)> {
        let query = self.input.read(cx).value();
        (!query.trim().is_empty()).then_some((self.current, self.matches.len()))
    }
}

struct Text(RenderedText);

impl AsRef<str> for Text {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

/// Every match of `query` in `items`, in transcript order: in messages (their trimmed text) and
/// in replies (their displayed text, from `reply`).
pub(crate) fn find_in<S: AsRef<str>>(items: &[TranscriptItem], query: &str, reply: impl Fn(usize) -> Option<S>) -> Vec<Match> {
    let mut out = Vec::new();
    for (ix, item) in items.iter().enumerate() {
        let found = match item {
            TranscriptItem::User { text, .. } => find_matches(text.trim(), query),
            TranscriptItem::Agent(_) => match reply(ix) {
                Some(text) => find_matches(text.as_ref(), query),
                None => continue,
            },
            _ => continue,
        };
        out.extend(found.into_iter().map(|range| Match { ix, range }));
    }
    out
}

/// Which match is current after searching again: the same one if it's still there, else the
/// next one after where it was, else the last.
fn keep_current(was: Option<&Match>, matches: &[Match]) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let Some(was) = was else { return Some(matches.len() - 1) };
    matches
        .iter()
        .position(|m| m == was)
        .or_else(|| matches.iter().position(|m| (m.ix, m.range.start) >= (was.ix, was.range.start)))
        .or(Some(matches.len() - 1))
}

impl SpaceApp {
    /// Before drawing the shown thread: its streaming reply (index, bytes to show), its replies'
    /// text views brought up to date, and find's matches if the bar is open.
    pub(crate) fn prepare_transcript(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<(usize, usize)> {
        let thread = self.current().filter(|t| !t.is_empty())?;
        let key = thread.key;
        let streaming = crate::view::streaming_reply(self, thread, window, cx);
        self.sync_replies(key, streaming, cx);
        self.sync_find(key, cx);
        streaming
    }

    fn sync_replies(&mut self, key: u64, streaming: Option<(usize, usize)>, cx: &mut Context<Self>) {
        let mut replies = std::mem::take(&mut self.replies);
        if replies.key != Some(key) {
            replies = Replies { key: Some(key), ..Default::default() };
        }
        let Some(thread) = self.thread(key) else {
            self.replies = replies;
            return;
        };
        let items = &thread.state.items;
        for (ix, item) in items.iter().enumerate() {
            let TranscriptItem::Agent(md) = item else { continue };
            let shown = match streaming {
                Some((at, n)) if at == ix => n.min(md.len()),
                _ => md.len(),
            };
            let seen = (md.len(), shown);
            let text = md.get(..shown).unwrap_or(md).trim();
            match replies.states.get(&ix) {
                Some(_) if replies.synced.get(&ix) == Some(&seen) => {}
                Some(state) => state.update(cx, |s, cx| s.set_text(text, cx)),
                None => {
                    let state = cx.new(|cx| TextViewState::markdown(text, cx));
                    replies.states.insert(ix, state);
                }
            }
            replies.synced.insert(ix, seen);
        }
        replies.states.retain(|ix, _| matches!(items.get(*ix), Some(TranscriptItem::Agent(_))));
        replies.synced.retain(|ix, _| replies.states.contains_key(ix));
        replies.marked.retain(|ix| replies.states.contains_key(ix));
        self.replies = replies;
    }

    /// Search again if the query or the thread's text changed; go to a match if asked.
    fn sync_find(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_ref().filter(|f| f.open) else { return };
        let query = find.input.read(cx).value().to_string();
        let Some(thread) = self.thread(key) else { return };
        let mut ixs: Vec<usize> = self.replies.states.keys().copied().collect();
        ixs.sort_unstable();
        let texts: Vec<RenderedText> = ixs.iter().map(|ix| self.replies.states[ix].read(cx).rendered_text()).collect();
        let seen = (query.clone(), key, thread.state.items.len(), texts);
        if find.seen.as_ref() == Some(&seen) {
            return;
        }
        let matches = find_in(&thread.state.items, &query, |ix| self.replies.states.get(&ix).map(|s| Text(s.read(cx).rendered_text())));
        let mut user: HashMap<usize, Vec<Range<usize>>> = HashMap::new();
        for m in &matches {
            if matches!(thread.state.items.get(m.ix), Some(TranscriptItem::User { .. })) {
                user.entry(m.ix).or_default().push(m.range.clone());
            }
        }
        let Some(find) = self.find.as_mut() else { return };
        let jump = std::mem::take(&mut find.jump);
        find.current = if jump { matches.len().checked_sub(1) } else { keep_current(find.current_match(), &matches) };
        find.user = user;
        find.matches = matches;
        find.seen = Some(seen);
        self.highlight_replies(cx);
        if jump {
            self.go_to_match(cx);
        }
    }

    /// Paint find's matches over the replies (the current one stronger); clear the rest.
    fn highlight_replies(&mut self, cx: &mut Context<Self>) {
        let mut by_reply: HashMap<usize, Vec<RangeHighlight>> = HashMap::new();
        if let Some(find) = self.find.as_ref().filter(|f| f.open) {
            for (i, m) in find.matches.iter().enumerate() {
                if self.replies.states.contains_key(&m.ix) {
                    let bg = if find.current == Some(i) { color::find_current() } else { color::find_match() };
                    by_reply.entry(m.ix).or_default().push(RangeHighlight::new(m.range.clone(), bg));
                }
            }
        }
        let replies = &mut self.replies;
        for (ix, state) in &replies.states {
            match by_reply.remove(ix) {
                Some(highlights) => {
                    state.update(cx, |s, cx| {
                        if let Err(e) = s.set_range_highlights(highlights, cx) {
                            tracing::debug!("find highlights: {e:?}");
                        }
                    });
                    replies.marked.insert(*ix);
                }
                None if replies.marked.remove(ix) => state.update(cx, |s, cx| s.clear_range_highlights(cx)),
                None => {}
            }
        }
    }

    /// Scroll to the current match, opening the long message it's in.
    fn go_to_match(&mut self, cx: &mut Context<Self>) {
        let Some(m) = self.find.as_ref().and_then(|f| f.current_match()).cloned() else { return };
        let Some(key) = self.selected else { return };
        let is_user = matches!(self.thread(key).and_then(|t| t.state.items.get(m.ix)), Some(TranscriptItem::User { .. }));
        if is_user {
            if let Some(t) = self.thread_mut(key) {
                t.expanded.insert(m.ix);
            }
            if let Some(f) = &self.find {
                f.reveal.set(true);
            }
        } else if let Some(state) = self.replies.states.get(&m.ix) {
            state.update(cx, |s, cx| {
                if let Err(e) = s.reveal_range(m.range.clone(), cx) {
                    tracing::debug!("find reveal: {e:?}");
                }
            });
        }
        cx.notify();
    }

    /// ⌘F: open the bar (or, open, select what's typed in it).
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings || self.current().is_none_or(|t| t.is_empty()) {
            return;
        }
        if self.find.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in thread"));
            let sub = cx.subscribe_in(&input, window, |this, _, e: &InputEvent, window, cx| match e {
                InputEvent::Change => {
                    if let Some(f) = this.find.as_mut() {
                        f.jump = true;
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { shift, .. } => this.find_step(!shift, window, cx),
                _ => {}
            });
            self.find = Some(Find {
                input,
                open: false,
                matches: Vec::new(),
                current: None,
                user: HashMap::new(),
                reveal: Rc::new(Cell::new(false)),
                seen: None,
                jump: false,
                _sub: sub,
            });
        }
        let Some(find) = self.find.as_mut() else { return };
        if !find.open {
            find.open = true;
            find.seen = None;
            find.jump = true;
        }
        find.input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        cx.notify();
    }

    /// The next (or previous) match, wrapping; ⌘G with the bar closed opens it.
    pub(crate) fn find_step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut().filter(|f| f.open) else {
            self.open_find(window, cx);
            return;
        };
        let n = find.matches.len();
        if n == 0 {
            return;
        }
        find.current = Some(match (find.current, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        });
        self.highlight_replies(cx);
        self.go_to_match(cx);
    }

    /// Esc or ✕: close the bar, clear the highlights, and (`refocus`) back to the composer.
    pub(crate) fn close_find(&mut self, refocus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut().filter(|f| f.open) else { return };
        find.open = false;
        find.matches.clear();
        find.user.clear();
        find.current = None;
        find.seen = None;
        find.reveal.set(false);
        self.highlight_replies(cx);
        if refocus {
            self.focus_composer(window, cx);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use space_core::TranscriptItem;

    use super::{Match, find_in, keep_current};

    fn user(text: &str) -> TranscriptItem {
        TranscriptItem::User { text: text.into(), quote: None, attachments: vec![], images: vec![], texts: vec![], at_ms: None }
    }

    #[test]
    fn finds_in_messages_and_replies_as_shown() {
        let items = vec![
            user("  Where are the Plugins?"),
            TranscriptItem::Thought("plugins in a thought".into()),
            TranscriptItem::Agent("The **plugins** live in `plugins/`.".into()),
            TranscriptItem::Error("plugin crashed".into()),
        ];
        // The reply as displayed: no markdown markers.
        let shown = |ix: usize| (ix == 2).then_some("The plugins live in plugins/.");
        let found = find_in(&items, "plugin", shown);
        assert_eq!(
            found,
            vec![
                Match { ix: 0, range: 14..20 },
                Match { ix: 2, range: 4..10 },
                Match { ix: 2, range: 20..26 },
            ]
        );
        // A message's ranges index its trimmed text.
        assert_eq!(&"Where are the Plugins?"[14..20], "Plugin");
        assert!(find_in(&items, "", shown).is_empty());
    }

    #[test]
    fn current_match_survives_new_text() {
        let m = |ix, start| Match { ix, range: start..start + 3 };
        let before = [m(0, 0), m(2, 5)];
        let after = vec![m(0, 0), m(2, 5), m(2, 40), m(4, 1)];
        assert_eq!(keep_current(before.get(1), &after), Some(1));
        // Gone (its text changed): the next one after it.
        assert_eq!(keep_current(Some(&m(2, 7)), &after), Some(2));
        assert_eq!(keep_current(Some(&m(9, 0)), &after), Some(3));
        assert_eq!(keep_current(None, &after), Some(3));
        assert_eq!(keep_current(Some(&m(0, 0)), &[]), None);
    }
}
