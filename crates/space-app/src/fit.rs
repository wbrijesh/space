//! Room for the chat. As the window narrows and the chat gets below [`size::CHAT_MIN_W`], the
//! right pane closes; narrower still, the sidebar. Widening reopens them in reverse (the
//! sidebar, then the pane), each once the chat keeps its minimum with it. Either one opened again
//! by hand stays open (and is no longer reopened), and the chat says it's too narrow, with a
//! button to hide each.

use gpui::*;
use space_ui::theme::size;

use crate::app::SpaceApp;

/// What narrowing the window closed, to reopen on widening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Closed {
    /// A thread's right pane.
    Pane(u64),
    Sidebar,
}

impl SpaceApp {
    /// The right pane beside the current thread's chat (open, not maximized over it).
    fn pane_beside(&self) -> bool {
        !self.show_settings && self.selected.and_then(|k| self.terminals.pane(k)).is_some_and(|p| p.open && !p.maximized)
    }

    /// The main column (chat and pane) beside the sidebar, where the sidebar is headed: the
    /// content panel's inside (its insets and border off).
    pub(crate) fn main_width(&self, window: &Window) -> f32 {
        let panel = 2. * f32::from(size::PANEL_INSET) + 2.;
        f32::from(window.viewport_size().width) - f32::from(size::SIDEBAR_W) * self.sidebar_shown.target() - panel
    }

    /// The pane's width as drawn: its set width, but never wider than the main column (a window
    /// narrower than the pane). The set width stays for when there's room again.
    pub(crate) fn pane_width(&self, window: &Window) -> f32 {
        self.terminals.width.min(self.main_width(window)).max(0.)
    }

    /// How wide the chat column is, with the sidebar and pane where they're headed.
    fn chat_width(&self, window: &Window) -> f32 {
        let main = self.main_width(window);
        if self.pane_beside() { main - self.pane_width(window) } else { main }
    }

    /// The window was resized. Narrower, with the chat under its minimum: close the pane, then
    /// the sidebar. Wider: reopen what that closed, last closed first, while the chat keeps its
    /// minimum.
    pub(crate) fn window_resized(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let width = f32::from(window.viewport_size().width);
        let before = std::mem::replace(&mut self.window_width, width);
        // Opened again by hand since: the user's call now.
        let (sidebar_open, panes) = (self.sidebar_open, &self.terminals);
        self.auto_closed.retain(|c| match c {
            Closed::Sidebar => !sidebar_open,
            Closed::Pane(k) => !panes.pane(*k).is_some_and(|p| p.open),
        });
        let min = f32::from(size::CHAT_MIN_W);
        if width < before {
            if self.chat_width(window) < min && self.pane_beside() {
                if let Some(key) = self.selected {
                    self.auto_closed.push(Closed::Pane(key));
                }
                self.hide_terminal(window, cx);
            }
            if self.chat_width(window) < min && self.sidebar_open {
                self.auto_closed.push(Closed::Sidebar);
                self.toggle_sidebar(cx);
            }
        } else if width > before {
            while let Some(&last) = self.auto_closed.last() {
                let chat = self.chat_width(window);
                let room = match last {
                    Closed::Sidebar => chat - f32::from(size::SIDEBAR_W),
                    // Another thread's pane takes no room from this chat.
                    Closed::Pane(k) if Some(k) == self.selected && !self.show_settings => chat - self.terminals.width,
                    Closed::Pane(_) => chat,
                };
                if room < min {
                    break;
                }
                self.auto_closed.pop();
                match last {
                    Closed::Sidebar => self.toggle_sidebar(cx),
                    Closed::Pane(k) => {
                        if let Some(pane) = self.terminals.panes.get_mut(&k) {
                            pane.open = true;
                        }
                        self.terminal_slide(cx);
                        cx.notify();
                    }
                }
            }
        }
    }

    /// A sidebar or pane opened by hand leaves the chat under its minimum.
    pub(crate) fn chat_too_narrow(&self, window: &Window) -> bool {
        (self.sidebar_open || self.pane_beside()) && self.chat_width(window) < f32::from(size::CHAT_MIN_W)
    }

    /// The overlay for [`Self::chat_too_narrow`]: hide whichever is open.
    pub(crate) fn too_narrow_overlay(&self, cx: &mut Context<Self>) -> Div {
        let sidebar = self.sidebar_open.then(|| crate::view::click(cx, |this, _, cx| this.toggle_sidebar(cx)));
        let pane = self.pane_beside().then(|| crate::view::click(cx, |this, w, cx| this.hide_terminal(w, cx)));
        space_ui::components::shell::too_narrow(sidebar, pane)
    }
}
