//! One terminal on screen: draws its [`Session`]'s screen and turns keys, pastes and the mouse
//! into bytes for its shell. It has its own key context (`Terminal`), so the composer's
//! shortcuts don't fire in it; ⌘ shortcuts that aren't terminal ones still reach the app.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use gpui::*;
use space_core::Host;
use space_terminal::{Key, Mods, MouseButton as TermButton, MouseEvent, SelectKind, Size as TermSize, paste_bytes};
use space_ui::actions::{TerminalClear, TerminalCopy, TerminalPaste};
use space_ui::components::terminal::{GridLayout, ended_notice, terminal_grid};

use crate::terminal::Session;

/// Asked of the app.
pub(crate) enum TerminalViewEvent {
    /// Enter in a terminal whose shell ended: a new shell in its place.
    Restart,
}

pub(crate) struct TerminalView {
    pub(crate) session: Arc<Session>,
    host: Host,
    pub(crate) focus: FocusHandle,
    layout: Rc<Cell<Option<GridLayout>>>,
    /// A drag is selecting text.
    selecting: bool,
    /// A button is down and its events go to the program.
    reporting: Option<TermButton>,
    /// Wheel movement not yet a whole line.
    wheel: f32,
    /// Focus as last told to the screen.
    focused: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

fn mods(m: &Modifiers) -> Mods {
    Mods { shift: m.shift, ctrl: m.control, alt: m.alt, cmd: m.platform }
}

fn button(b: MouseButton) -> Option<TermButton> {
    match b {
        MouseButton::Left => Some(TermButton::Left),
        MouseButton::Middle => Some(TermButton::Middle),
        MouseButton::Right => Some(TermButton::Right),
        _ => None,
    }
}

/// A GPUI key to a terminal key.
fn key(k: &str) -> Option<Key> {
    Some(match k {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "space" => Key::Char(' '),
        f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => Key::F(f[1..].parse().ok()?),
        c if c.chars().count() == 1 => Key::Char(c.chars().next()?),
        _ => return None,
    })
}

impl TerminalView {
    pub(crate) fn new(session: Arc<Session>, host: Host, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        // The cursor's look and focus reports follow focus.
        let subscriptions = vec![
            cx.on_focus_in(&focus, window, |this, _, cx| this.focus_changed(true, cx)),
            cx.on_focus_out(&focus, window, |this, _, _, cx| this.focus_changed(false, cx)),
        ];
        Self { session, host, focus, layout: Rc::default(), selecting: false, reporting: None, wheel: 0., focused: false, _subscriptions: subscriptions }
    }

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        if focused != self.focused {
            self.focused = focused;
            let reply = self.session.screen.lock().unwrap().set_focused(focused);
            if let Some(bytes) = reply {
                self.send(bytes);
            }
        }
        cx.notify();
    }

    fn send(&self, bytes: Vec<u8>) {
        self.host.terminal_input(&self.session.id, bytes);
    }

    /// Typing or pasting goes back to the bottom.
    fn send_typed(&self, bytes: Vec<u8>) {
        self.session.screen.lock().unwrap().scroll_to_bottom();
        self.send(bytes);
    }

    fn on_key(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let ks = &e.keystroke;
        if self.session.ended().is_some() {
            if ks.key == "enter" && !ks.modifiers.modified() {
                cx.emit(TerminalViewEvent::Restart);
                cx.stop_propagation();
            }
            return;
        }
        let m = mods(&ks.modifiers);
        let Some(k) = key(&ks.key) else { return };
        let bytes = match (&k, &ks.key_char) {
            // Plain typing: the character the keyboard layout makes (shifted, dead keys done).
            (Key::Char(_), Some(text)) if !m.ctrl && !m.alt && !m.cmd => Some(text.clone().into_bytes()),
            _ => self.session.screen.lock().unwrap().key(&k, m),
        };
        let Some(bytes) = bytes else { return };
        cx.stop_propagation();
        self.send_typed(bytes);
        cx.notify();
    }

    fn copy(&mut self, _: &TerminalCopy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.session.screen.lock().unwrap().selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &TerminalPaste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else { return };
        if self.session.ended().is_some() {
            return;
        }
        let bracketed = self.session.screen.lock().unwrap().bracketed_paste();
        self.send_typed(paste_bytes(&text, bracketed));
    }

    fn clear(&mut self, _: &TerminalClear, _: &mut Window, cx: &mut Context<Self>) {
        self.session.screen.lock().unwrap().clear_scrollback();
        // ^L: the shell clears the screen and draws its prompt again.
        self.send(vec![0x0c]);
        cx.notify();
    }

    fn cell(&self, at: Point<Pixels>) -> Option<(u16, u16, bool)> {
        self.layout.get().map(|l| l.cell_at(at))
    }

    fn mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let Some((col, row, right)) = self.cell(e.position) else { return };
        let m = mods(&e.modifiers);
        let mut screen = self.session.screen.lock().unwrap();
        if e.button == MouseButton::Left && m.cmd {
            if let Some(url) = screen.url_at(col, row) {
                cx.open_url(&url);
            }
            return;
        }
        // A program that takes the mouse gets it, unless ⌥ is held (select anyway).
        if screen.mouse_mode() && !m.alt {
            if let Some(b) = button(e.button) {
                let report = screen.mouse(MouseEvent { button: b, pressed: true, motion: false, col, row, mods: m });
                drop(screen);
                self.reporting = Some(b);
                if let Some(bytes) = report {
                    self.send(bytes);
                }
            }
            return;
        }
        if e.button != MouseButton::Left {
            return;
        }
        if m.shift && screen.selection_text().is_some() {
            screen.extend_selection(col, row, right);
        } else {
            let kind = match e.click_count {
                2 => SelectKind::Word,
                n if n >= 3 => SelectKind::Line,
                _ => SelectKind::Simple,
            };
            screen.select(kind, col, row, right);
        }
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some((col, row, right)) = self.cell(e.position) else { return };
        if let Some(b) = self.reporting {
            let report = self.session.screen.lock().unwrap().mouse(MouseEvent { button: b, pressed: true, motion: true, col, row, mods: mods(&e.modifiers) });
            if let Some(bytes) = report {
                self.send(bytes);
            }
            return;
        }
        if self.selecting && e.pressed_button == Some(MouseButton::Left) {
            self.session.screen.lock().unwrap().extend_selection(col, row, right);
            cx.notify();
            return;
        }
        if e.pressed_button.is_none() {
            let screen = self.session.screen.lock().unwrap();
            if screen.mouse_mode() {
                let report = screen.mouse(MouseEvent { button: TermButton::None, pressed: false, motion: true, col, row, mods: mods(&e.modifiers) });
                drop(screen);
                if let Some(bytes) = report {
                    self.send(bytes);
                }
            }
        }
    }

    fn mouse_up(&mut self, e: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let cell = self.cell(e.position);
        if let (Some(b), Some((col, row, _))) = (self.reporting.take(), cell) {
            let report = self.session.screen.lock().unwrap().mouse(MouseEvent { button: b, pressed: false, motion: false, col, row, mods: mods(&e.modifiers) });
            if let Some(bytes) = report {
                self.send(bytes);
            }
        }
        if std::mem::take(&mut self.selecting) {
            let mut screen = self.session.screen.lock().unwrap();
            // A click without a drag selects nothing.
            if e.click_count == 1 && screen.selection_text().is_none() {
                screen.clear_selection();
            }
            cx.notify();
        }
    }

    fn scroll(&mut self, e: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = self.layout.get() else { return };
        let (col, row, _) = layout.cell_at(e.position);
        let dy = f32::from(e.delta.pixel_delta(layout.cell.height).y);
        self.wheel += dy / f32::from(layout.cell.height).max(1.);
        let lines = self.wheel.trunc();
        self.wheel -= lines;
        if lines != 0. {
            let reply = self.session.screen.lock().unwrap().wheel(lines as i32, col, row, mods(&e.modifiers));
            if let Some(bytes) = reply {
                self.send(bytes);
            }
            cx.notify();
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.clone();
        session.redraw.store(false, Ordering::SeqCst);
        session.unread.store(false, Ordering::SeqCst);
        let ended = session.ended();
        let mut frame = session.screen.lock().unwrap().frame();
        // Nothing reads input once the shell ended.
        if ended.is_some() {
            frame.cursor = None;
        }
        let frame = Arc::new(frame);
        let focused = self.focus.is_focused(window);
        let on_layout = {
            let (layout, session, host) = (self.layout.clone(), session.clone(), self.host.clone());
            Rc::new(move |l: GridLayout, window: &mut Window, _: &mut App| {
                layout.set(Some(l));
                let mut screen = session.screen.lock().unwrap();
                let size = TermSize { cols: l.cols, rows: l.rows, cell_width: f32::from(l.cell.width), cell_height: f32::from(l.cell.height) };
                let old = screen.size();
                if (old.cols, old.rows) != (size.cols, size.rows) {
                    screen.resize(size);
                    drop(screen);
                    session.last_size.store(((l.cols as u32) << 16) | l.rows as u32, Ordering::SeqCst);
                    host.terminal_resize(&session.id, l.cols, l.rows);
                    // This frame was laid out for the old size.
                    window.refresh();
                } else {
                    screen.resize(size);
                }
            })
        };
        div()
            .id(SharedString::from(format!("terminal-{}", session.id)))
            .track_focus(&self.focus)
            .key_context("Terminal")
            .size_full()
            .flex()
            .flex_col()
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::on_key))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::clear))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll))
            .child(div().flex_1().min_h_0().child(terminal_grid(frame, focused, on_layout)))
            .children(ended.map(ended_notice))
    }
}
