//! Finished turns fold their work: once a turn ends normally, its thinking and tool calls (and
//! the answered permission and plan cards among them) fold under one "Completed in 2m 3s" row
//! right after your message. Replies along the way, the final reply and errors stay visible.
//! Failed, stopped and running turns stay open, as does a turn whose only work was thinking.
//!
//! Also [`StreamReveal`]: the streaming reply shown at an even pace instead of in bursts.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::component::Icon;
use gpui::assets::IconName as Lucide;
use gpui::*;
use space_core::TranscriptItem;

use crate::theme::{color, radius, size, text};

pub type TurnToggleHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// A finished turn that folds: the index of your message, of its end, and how long it took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnFold {
    pub user: usize,
    pub end: usize,
    pub duration_ms: Option<u64>,
}

/// What a folded turn hides.
pub fn folds_away(item: &TranscriptItem) -> bool {
    matches!(
        item,
        TranscriptItem::Thought(_) | TranscriptItem::Tool(_) | TranscriptItem::Plan(_) | TranscriptItem::Permission { resolved: Some(_), .. }
    )
}

/// The turns of `items` that fold, by the index of their user message: ended `end_turn`, with at
/// least one tool call, and no permission still waiting.
pub fn foldable_turns(items: &[TranscriptItem]) -> HashMap<usize, TurnFold> {
    let mut out = HashMap::new();
    for (user, item) in items.iter().enumerate() {
        let TranscriptItem::User { at_ms: started, .. } = item else { continue };
        let mut tools = false;
        let mut waiting = false;
        for (ix, next) in items.iter().enumerate().skip(user + 1) {
            match next {
                TranscriptItem::User { .. } => break,
                TranscriptItem::Tool(_) => tools = true,
                TranscriptItem::Permission { resolved: None, .. } => waiting = true,
                TranscriptItem::TurnEnd { reason, at_ms: ended, .. } => {
                    if reason == "end_turn" && tools && !waiting {
                        let duration_ms = started.zip(*ended).map(|(s, e)| e.saturating_sub(s));
                        out.insert(user, TurnFold { user, end: ix, duration_ms });
                    }
                    break;
                }
                _ => {}
            }
        }
    }
    out
}

/// "45s", "2m 3s", "2m", "1h 5m": at most two units.
pub fn short_span(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0 => "<1s".to_string(),
        1..60 => format!("{s}s"),
        60..3600 => match s % 60 {
            0 => format!("{}m", s / 60),
            r => format!("{}m {r}s", s / 60),
        },
        _ => match (s % 3600) / 60 {
            0 => format!("{}h", s / 3600),
            m => format!("{}h {m}m", s / 3600),
        },
    }
}

/// "Completed in 2m 3s", or "Completed" when the turn's times aren't known.
pub fn completed_label(duration_ms: Option<u64>) -> String {
    duration_ms.map_or_else(|| "Completed".to_string(), |ms| format!("Completed in {}", short_span(ms)))
}

/// The row a folded turn shows in place of its work; a click (or Return) opens or folds it.
pub fn turn_summary_row(
    user_ix: usize,
    duration_ms: Option<u64>,
    open: bool,
    window: &mut Window,
    cx: &mut App,
    on_toggle: TurnToggleHandler,
) -> Stateful<Div> {
    let reduce_motion = cx.reduce_motion();
    // Focusable, so Return toggles it after a click (or Tab) focused it.
    let focus = window.use_keyed_state(("turn-summary-focus", user_ix), cx, |_, cx| cx.focus_handle()).read(cx).clone();
    let toggle = on_toggle.clone();
    let focus_for_click = focus.clone();
    // The chevron turns from right to down; the animation restarts when `open` flips.
    let (from, to) = if open { (0., 90.) } else { (90., 0.) };
    let chevron = Icon::new(Lucide::ChevronRight).size(size::ICON_SM);
    let chevron: AnyElement = if reduce_motion {
        chevron.rotate(Radians(to_radians(to))).into_any_element()
    } else {
        chevron
            .with_animation(
                ElementId::Name(format!("turn-chevron-{user_ix}-{open}").into()),
                Animation::new(std::time::Duration::from_millis(120)).with_easing(ease_out_quint()),
                move |icon, t| icon.rotate(Radians(to_radians(from + (to - from) * t))),
            )
            .into_any_element()
    };
    div()
        .id(("turn-summary", user_ix))
        .track_focus(&focus)
        .flex()
        .items_center()
        .gap_1p5()
        .h(size::ROW_H)
        .px_2()
        .rounded(radius::LG)
        .text_size(text::SM)
        .text_color(color::text_3())
        .font_features(FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])))
        .cursor_pointer()
        .hover(|s| s.text_color(color::text_2()).bg(color::surface()))
        .child(Icon::new(Lucide::ListChecks).size(size::ICON))
        .child(completed_label(duration_ms))
        .child(chevron)
        .on_click(move |_, w, cx| {
            w.focus(&focus_for_click, cx);
            toggle(user_ix, w, cx)
        })
        .on_key_down(move |e, w, cx| {
            if e.keystroke.key == "enter" {
                on_toggle(user_ix, w, cx);
            }
        })
}

fn to_radians(degrees: f32) -> f32 {
    degrees.to_radians()
}

// ----- streaming pace ----------------------------------------------------------------------------

/// The streaming reply shown at an even pace: the shown part catches up with what has arrived in
/// about [`StreamReveal::LAG`] seconds, and never slower than [`StreamReveal::MIN_CPS`], so a
/// burst flows in instead of appearing at once. A reply that was already there (another thread
/// opened, a reload) shows whole.
#[derive(Debug, Clone, Default)]
pub struct StreamReveal {
    /// The reply being revealed: (thread, item index).
    item: Option<(u64, usize)>,
    /// Characters shown.
    shown: f64,
    /// Animation time of the last step.
    last: Option<f64>,
}

impl StreamReveal {
    /// Seconds the shown text trails what arrived.
    pub const LAG: f64 = 0.2;
    /// Characters per second at the least.
    pub const MIN_CPS: f64 = 240.;

    /// How many bytes of `full` (item `ix` of thread `key`) to show at animation time `now`, and
    /// whether more is still to come. `instant`: show it all (reduced motion, a pinned clock).
    pub fn advance(&mut self, key: u64, ix: usize, full: &str, now: f64, instant: bool) -> (usize, bool) {
        let total = full.chars().count() as f64;
        match self.item {
            Some((k, i)) if k == key && i == ix => {}
            // Another reply of the same thread starts from nothing; one already there (another
            // thread opened) shows whole.
            Some((k, _)) if k == key => self.shown = 0.,
            _ => self.shown = total,
        }
        self.item = Some((key, ix));
        let dt = self.last.map_or(0., |l| (now - l).clamp(0., 0.1));
        self.last = Some(now);
        if instant {
            self.shown = total;
        } else if self.shown < total {
            let backlog = total - self.shown;
            self.shown = (self.shown + (Self::MIN_CPS * dt).max(backlog * dt / Self::LAG)).min(total);
        }
        let chars = self.shown.floor() as usize;
        let bytes = full.char_indices().nth(chars).map_or(full.len(), |(b, _)| b);
        (bytes, bytes < full.len())
    }

    /// Nothing streams in the shown thread: the next reply of thread `key` starts from nothing.
    pub fn idle(&mut self, key: u64) {
        self.item = Some((key, usize::MAX));
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use space_core::{PermissionDecision, PermissionRequest, ToolCallInfo, ToolCallPatch, ToolStatus, TranscriptItem};

    use super::{StreamReveal, completed_label, folds_away, foldable_turns, short_span};

    fn user(at: Option<u64>) -> TranscriptItem {
        TranscriptItem::User { text: "do it".into(), attachments: vec![], images: vec![], texts: vec![], at_ms: at }
    }
    fn tool() -> TranscriptItem {
        TranscriptItem::Tool(ToolCallInfo {
            id: "t".into(),
            title: "bash: ls".into(),
            kind: "execute".into(),
            status: ToolStatus::Completed,
            output: String::new(),
            locations: vec![],
            blocked: None,
            held: None,
        })
    }
    fn end(reason: &str, at: Option<u64>) -> TranscriptItem {
        TranscriptItem::TurnEnd { reason: reason.into(), at_ms: at, usage: None }
    }
    fn permission(resolved: Option<PermissionDecision>) -> TranscriptItem {
        TranscriptItem::Permission {
            request: PermissionRequest { request_id: 1, tool_call: ToolCallPatch::default(), options: vec![], reason: None },
            resolved,
        }
    }

    #[test]
    fn spans_use_at_most_two_units() {
        let cases = [(0, "<1s"), (999, "<1s"), (45_000, "45s"), (123_000, "2m 3s"), (120_000, "2m"), (3_900_000, "1h 5m"), (7_200_000, "2h")];
        for (ms, want) in cases {
            assert_eq!(short_span(ms), want, "{ms}");
        }
        assert_eq!(completed_label(Some(123_000)), "Completed in 2m 3s");
        assert_eq!(completed_label(None), "Completed");
    }

    #[test]
    fn only_normally_finished_turns_with_tools_fold() {
        use TranscriptItem::{Agent, Thought};
        let items = vec![
            // 0..=5: folds (thinking, a tool, a reply along the way, the final reply).
            user(Some(1_000)),
            Thought("hm".into()),
            tool(),
            Agent("checking".into()),
            Agent("done".into()),
            end("end_turn", Some(124_000)),
            // 6..=8: only thinking: stays open.
            user(None),
            Thought("hm".into()),
            end("end_turn", None),
            // 9..=11: stopped: stays open.
            user(None),
            tool(),
            end("cancelled", None),
            // 12..=14: failed: stays open.
            user(None),
            tool(),
            end("error", None),
            // 15..=17: no times: folds as "Completed".
            user(None),
            tool(),
            end("end_turn", None),
            // 18..: running (no end): stays open.
            user(None),
            tool(),
        ];
        let folds = foldable_turns(&items);
        let mut users: Vec<_> = folds.keys().copied().collect();
        users.sort();
        assert_eq!(users, [0, 15]);
        assert_eq!(folds[&0].end, 5);
        assert_eq!(folds[&0].duration_ms, Some(123_000));
        assert_eq!(folds[&15].duration_ms, None);
        // What folds away: thinking and tools, not replies.
        assert!(folds_away(&items[1]) && folds_away(&items[2]));
        assert!(!folds_away(&items[3]) && !folds_away(&items[4]));
    }

    #[test]
    fn a_waiting_permission_keeps_the_turn_open_and_answered_ones_fold() {
        let items = vec![user(None), tool(), permission(None), end("end_turn", None)];
        assert!(foldable_turns(&items).is_empty());
        let answered = permission(Some(PermissionDecision::Selected("allow".into())));
        assert!(folds_away(&answered));
        let items = vec![user(None), tool(), answered, end("end_turn", None)];
        assert_eq!(foldable_turns(&items).len(), 1);
    }

    #[test]
    fn reveal_paces_bursts_and_shows_existing_replies_whole() {
        let mut r = StreamReveal::default();
        // A reply already there when the thread is opened: whole.
        let text = "x".repeat(400);
        assert_eq!(r.advance(1, 3, &text, 0.0, false), (400, false));
        // Its next reply starts from nothing and catches up in about LAG.
        r.idle(1);
        let (b0, more0) = r.advance(1, 5, &text, 1.0, false);
        assert_eq!((b0, more0), (0, true));
        let (b1, more1) = r.advance(1, 5, &text, 1.016, false);
        assert!(b1 > 0 && b1 < 400 && more1, "{b1}");
        let mut t = 1.016;
        let mut shown = b1;
        while shown < 400 {
            t += 0.016;
            shown = r.advance(1, 5, &text, t, false).0;
        }
        assert!(t - 1.0 < 1.0, "caught up in {:.2}s", t - 1.0);
        // Multi-byte characters: never splits one.
        let mut r = StreamReveal::default();
        r.idle(2);
        let accents = "é".repeat(50);
        r.advance(2, 1, &accents, 0.0, false);
        let (b, _) = r.advance(2, 1, &accents, 0.05, false);
        assert!(accents.is_char_boundary(b));
        // Instant (reduced motion, pinned clock): everything.
        assert_eq!(r.advance(2, 1, &accents, 0.06, true), (accents.len(), false));
    }
}
