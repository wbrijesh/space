//! Quoting the transcript. Selecting text in a message offers a "Quote" button just above the
//! selection's end; it puts the selection in the composer (one quote at a time), and the agent
//! gets it before the message as a blockquote.
//!
//! What's offered is settled two frames after each mouse press, release or scroll (once the
//! text has painted and projected the selection): the selection, if it's finished and ends
//! inside the transcript. Where the button floats is worked out at each render from where the
//! selection's end was last painted, so it follows the text. A press on the button clears the
//! selection before the button sees it, so it quotes the text kept when the offer was settled.

use std::cell::Cell;
use std::rc::Rc;

use gpui::base::TextSelection;
use gpui::*;
use space_ui::components::quote::quote_button;

use crate::app::SpaceApp;

/// The transcript's bounds as last painted (where a quotable selection must end).
pub(crate) type Area = Rc<Cell<Option<Bounds<Pixels>>>>;

impl SpaceApp {
    /// Settle what's offered once the press, release or scroll has been handled and painted.
    pub(crate) fn refresh_quote_offer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        window.on_next_frame(move |window, _| {
            window.refresh();
            window.on_next_frame(move |window, cx| {
                let _ = this.update(cx, |this, cx| this.settle_quote_offer(window, cx));
            });
        });
    }

    fn settle_quote_offer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let offer = self.selected.and_then(|key| {
            let end = TextSelection::end_caret(window, cx)?;
            self.transcript_area.get().filter(|a| a.contains(&end.center()))?;
            let text = TextSelection::selected_text(window, cx);
            let text = text.trim();
            (!text.is_empty()).then(|| (key, text.to_string()))
        });
        if offer != self.quote_offer {
            self.quote_offer = offer;
            cx.notify();
        }
    }

    /// The Quote button: the selection goes into the composer, replacing any quote there.
    pub(crate) fn quote_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((key, text)) = self.quote_offer.take() else { return };
        if let Some(t) = self.thread_mut(key) {
            t.quote = Some(text);
        }
        TextSelection::clear(window, cx);
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub(crate) fn remove_quote(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.selected.and_then(|key| self.thread_mut(key)) {
            t.quote = None;
        }
        cx.notify();
    }

    /// The floating button, just above the end of the selection it quotes, while that end is
    /// in the shown thread's transcript.
    pub(crate) fn quote_button(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (key, _) = self.quote_offer.as_ref()?;
        if self.selected != Some(*key) || self.paste_view.is_some() || self.viewer.is_some() {
            return None;
        }
        let end = TextSelection::end_caret(window, cx)?;
        self.transcript_area.get().filter(|a| a.contains(&end.center()))?;
        let at = point(end.right() - px(14.), end.top() - px(6.));
        let this = cx.entity().downgrade();
        let button = quote_button().on_mouse_down(MouseButton::Left, move |_, w, cx| {
            cx.stop_propagation();
            let _ = this.update(cx, |this, cx| this.quote_selection(w, cx));
        });
        Some(deferred(anchored().anchor(Anchor::BottomLeft).position(at).snap_to_window_with_margin(px(8.)).child(button)).with_priority(1).into_any_element())
    }
}
