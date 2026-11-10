//! Jumping to threads from the keyboard.
//!
//! Holding ⌘ numbers the sidebar rows in view, from the top of what's shown (1, 2, … 10, 11 …:
//! scrolling renumbers them). Typing a number while ⌘ is held picks a row, and letting go of ⌘
//! goes to it, so two digits work (⌘ held, 1, 2, release: row 12). Rows the digits typed so far
//! no longer lead to fade. Any other ⌘ shortcut (⌘J, ⌘N…) hides the numbers at once, until ⌘
//! is pressed again. ⌘⌥↓ / ⌘⌥↑ step to the next or previous thread in the list.

use std::ops::Range;

use gpui::*;

use crate::app::SpaceApp;

/// ⌘ and a digit, nothing else: a thread number.
pub(crate) fn is_number_key(k: &Keystroke) -> bool {
    let m = &k.modifiers;
    m.platform && !m.alt && !m.control && !m.shift && k.key.len() == 1 && k.key.chars().all(|c| c.is_ascii_digit())
}

impl SpaceApp {
    /// Another ⌘ shortcut: hide the numbers and forget the digits until ⌘ is pressed again.
    pub(crate) fn cmd_shortcut(&mut self, cx: &mut Context<Self>) {
        if !self.cmd_other || !self.cmd_digits.is_empty() {
            self.cmd_other = true;
            self.cmd_digits.clear();
            cx.notify();
        }
    }

    /// The listed threads, in sidebar order (a new thread with nothing sent isn't listed).
    pub(crate) fn listed_threads(&self) -> Vec<u64> {
        self.threads.iter().rev().filter(|t| !t.is_empty()).map(|t| t.key).collect()
    }

    /// The listed rows in view (as last laid out); all of them before the first layout. A row
    /// counts when its middle is in view.
    fn rows_in_view(&self, count: usize) -> Range<usize> {
        let view = self.sidebar_scroll.bounds();
        if view.size.height <= px(0.) {
            return 0..count;
        }
        let (top, bottom) = (view.top(), view.bottom());
        let shown = |ix: usize| self.sidebar_scroll.bounds_for_item(ix).is_some_and(|b| (top..=bottom).contains(&b.center().y));
        match (0..count).find(|ix| shown(*ix)) {
            Some(first) => first..(first..count).take_while(|ix| shown(*ix)).last().map_or(first, |l| l + 1),
            None => 0..0,
        }
    }

    /// While ⌘ alone is held (and the sidebar is open): each listed row's number, if it has
    /// one, and whether the digits typed so far lead to it.
    pub(crate) fn row_shortcuts(&self) -> Vec<Option<(SharedString, bool)>> {
        let count = self.listed_threads().len();
        if !self.cmd_alone || self.cmd_other || !self.sidebar_open {
            return vec![None; count];
        }
        let view = self.rows_in_view(count);
        (0..count)
            .map(|ix| {
                view.contains(&ix).then(|| {
                    let number = (ix - view.start + 1).to_string();
                    let matches = number.starts_with(&self.cmd_digits);
                    (number.into(), matches)
                })
            })
            .collect()
    }

    /// A key went down with ⌘ held: a digit adds to the number (two at most: a third starts
    /// over); anything else is a shortcut, which forgets it. `true`: the digit was taken.
    pub(crate) fn cmd_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        if is_number_key(keystroke) && !self.cmd_other && self.sidebar_open && !self.listed_threads().is_empty() {
            if self.cmd_digits.len() >= 2 {
                self.cmd_digits.clear();
            }
            self.cmd_digits.push_str(&keystroke.key);
            cx.notify();
            return true;
        }
        if !self.cmd_digits.is_empty() {
            self.cmd_digits.clear();
            cx.notify();
        }
        false
    }

    /// ⌘ was let go: go to the row whose number was typed (if any).
    pub(crate) fn cmd_released(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let digits = std::mem::take(&mut self.cmd_digits);
        let Ok(n) = digits.parse::<usize>() else { return };
        let listed = self.listed_threads();
        let view = self.rows_in_view(listed.len());
        if n == 0 || n > view.len() {
            return;
        }
        let key = listed[view.start + n - 1];
        self.select_thread(key, window, cx);
    }

    /// ⌘⌥↓ (`delta` 1) / ⌘⌥↑ (-1): the next or previous thread in the list. From a new thread
    /// (not listed), ↓ goes to the first.
    pub(crate) fn step_thread(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let listed = self.listed_threads();
        let at = self.selected.filter(|_| !self.show_settings).and_then(|k| listed.iter().position(|l| *l == k));
        let next = match at {
            Some(ix) => ix.checked_add_signed(delta).filter(|n| *n < listed.len()),
            None if delta > 0 => (!listed.is_empty()).then_some(0),
            None => None,
        };
        if let Some(ix) = next {
            self.select_thread(listed[ix], window, cx);
            self.sidebar_scroll.scroll_to_item(ix);
        }
    }
}
