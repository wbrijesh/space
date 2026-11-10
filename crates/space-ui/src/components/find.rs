//! Find in thread (⌘F): the bar at the top right of the transcript, and the matching it uses.
//!
//! The bar is a search field, "n of m", previous and next, and close. The app finds the
//! matches (in your messages and the agent's replies, as displayed) with [`find_matches`] and
//! highlights them; see `space-app`'s `find` module.

use std::ops::Range;
use std::rc::Rc;

use gpui::component::input::{Input, InputState};
use gpui::component::{Icon, IconName, Sizable as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::tip::TipExt as _;
use super::widgets::icon_button;
use crate::actions::{CloseFind, FindNext, FindPrevious};
use crate::theme::{color, radius, size, text};

/// The key context of the bar (Enter, ⇧Enter and Esc in its field).
pub const CONTEXT: &str = "FindBar";

const BAR_W: Pixels = px(340.);

/// Every match of `needle` in `hay`, ignoring case, as byte ranges of `hay`, in order and not
/// overlapping. An empty (or all-space) needle matches nothing.
pub fn find_matches(hay: &str, needle: &str) -> Vec<Range<usize>> {
    let needle: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    if needle.iter().all(|c| c.is_whitespace()) {
        return Vec::new();
    }
    // `hay` lowercased char by char, each with the byte range of the char it came from (one
    // char can lowercase to several).
    let mut low: Vec<(char, usize, usize)> = Vec::with_capacity(hay.len());
    for (at, c) in hay.char_indices() {
        let end = at + c.len_utf8();
        for l in c.to_lowercase() {
            low.push((l, at, end));
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= low.len() {
        if low[i..i + needle.len()].iter().zip(&needle).all(|(h, n)| h.0 == *n) {
            let (start, end) = (low[i].1, low[i + needle.len() - 1].2);
            // A match must start and end on whole chars of `hay`.
            if (i == 0 || low[i - 1].1 != start) && low.get(i + needle.len()).is_none_or(|n| n.1 != low[i + needle.len() - 1].1) {
                out.push(start..end);
                i += needle.len();
                continue;
            }
        }
        i += 1;
    }
    out
}

pub type StepHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;
pub type CloseHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The bar. `count`: the current match (0-based) and how many there are; `None` while the
/// field is empty.
pub fn find_bar(input: &Entity<InputState>, count: Option<(Option<usize>, usize)>, on_step: StepHandler, on_close: CloseHandler) -> Div {
    let label = match count {
        None => String::new(),
        Some((_, 0)) => "No results".to_string(),
        Some((Some(at), n)) => format!("{} of {n}", at + 1),
        Some((None, n)) => format!("{n} found"),
    };
    let any = count.is_some_and(|(_, n)| n > 0);
    let step = |id: &'static str, icon: IconName, forward: bool, tip: &'static str, action: &dyn Action| {
        let on_step = on_step.clone();
        icon_button(id, icon, any, false).when(any, |b| b.on_click(move |_, w, cx| on_step(forward, w, cx))).tip_action(tip, action)
    };
    div()
        .key_context(CONTEXT)
        .w(BAR_W)
        .flex()
        .items_center()
        .gap_1()
        .p_1()
        .rounded(radius::CARD)
        .bg(color::surface())
        .border_1()
        .border_color(color::border())
        .shadow_md()
        // Clicks on the bar stay on it (the window's focus sink would take the field's focus).
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div().flex_1().min_w_0().child(
                Input::new(input)
                    .small()
                    .appearance(false)
                    .prefix(Icon::new(IconName::Search).size(size::ICON_SM).text_color(color::text_3())),
            ),
        )
        .child(
            div()
                .flex_none()
                .px_1()
                .text_size(text::SM)
                .text_color(if count.is_some_and(|(_, n)| n == 0) { color::text_3() } else { color::text_2() })
                .child(label),
        )
        .child(step("find-previous", IconName::ChevronUp, false, "Previous match", &FindPrevious))
        .child(step("find-next", IconName::ChevronDown, true, "Next match", &FindNext))
        .child(icon_button("find-close", IconName::Close, true, false).on_click(move |_, w, cx| on_close(w, cx)).tip_action("Close", &CloseFind))
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    let bar = |name: &'static str, query: &'static str, count: Option<(Option<usize>, usize)>| {
        Example::new(name, move |window, cx| {
            let input = window.use_keyed_state(SharedString::from(format!("find-{name}")), cx, |window, cx| {
                InputState::new(window, cx).placeholder("Find in thread").default_value(query)
            });
            find_bar(&input, count, Rc::new(|_, _, _| {}), Rc::new(|_, _| {})).into_any_element()
        })
    };
    vec![ComponentPreview::new(Area::Transcript, "Find bar", "⌘F: find in your messages and the agent's replies.").examples([
        bar("empty", "", None),
        bar("matches", "plugin", Some((Some(2), 12))),
        bar("no results", "kubernetes", Some((None, 0))),
    ])]
}

#[cfg(test)]
mod tests {
    use super::find_matches;

    #[test]
    fn matches_ignore_case() {
        assert_eq!(find_matches("Plugin plugins PLUGIN", "plugin"), vec![0..6, 7..13, 15..21]);
        assert_eq!(find_matches("aaaa", "aa"), vec![0..2, 2..4]);
        assert!(find_matches("abc", "").is_empty());
        assert!(find_matches("abc", "  ").is_empty());
        assert!(find_matches("ab", "abc").is_empty());
    }

    #[test]
    fn matches_are_byte_ranges_of_the_text() {
        let hay = "Café — CAFÉ";
        let found = find_matches(hay, "café");
        assert_eq!(found.len(), 2);
        assert_eq!(&hay[found[0].clone()], "Café");
        assert_eq!(&hay[found[1].clone()], "CAFÉ");
        // İ lowercases to two chars: matching half of it doesn't count.
        assert!(find_matches("İ", "i").is_empty());
        assert_eq!(find_matches("İx", "i\u{307}x"), vec![0.."İx".len()]);
    }
}
