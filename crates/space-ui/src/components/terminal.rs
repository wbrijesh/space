//! The terminal pane's pieces: the grid (a [`Frame`] painted cell by cell in the code font),
//! the tab strip, and the notices for a terminal whose shell ended or whose plugin stopped.

use std::rc::Rc;
use std::sync::Arc;

use gpui::assets::IconName as Lucide;
use gpui::component::Icon;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_terminal::{CursorShape, Frame, FrameCell};

use super::tip::TipExt as _;
use super::widgets::{icon_button, pill_button};
use crate::actions::{MaximizeTerminal, NewTerminal, ToggleTerminal};
use crate::theme::{color, radius, size, text};

/// The terminal's text size.
pub const FONT_SIZE: Pixels = px(12.5);
/// Space between the pane's edges and the grid.
pub const PAD: Pixels = px(10.);
/// The tab strip's height: the same as the Chat | Trajectory bar beside it.
pub const STRIP_H: Pixels = size::PANE_BAR_H;

/// Where the grid sits and how big its cells are (for turning pointer positions into cells).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridLayout {
    pub origin: Point<Pixels>,
    pub cell: Size<Pixels>,
    pub cols: u16,
    pub rows: u16,
}

impl GridLayout {
    /// The cell under a window position, and whether it's in the cell's right half.
    pub fn cell_at(&self, at: Point<Pixels>) -> (u16, u16, bool) {
        let x = f32::from(at.x - self.origin.x).max(0.);
        let y = f32::from(at.y - self.origin.y).max(0.);
        let (w, h) = (f32::from(self.cell.width).max(1.), f32::from(self.cell.height).max(1.));
        let col = ((x / w) as u16).min(self.cols.saturating_sub(1));
        let row = ((y / h) as u16).min(self.rows.saturating_sub(1));
        (col, row, x - col as f32 * w > w / 2.)
    }
}

pub type LayoutHandler = Rc<dyn Fn(GridLayout, &mut Window, &mut App)>;

fn mono(bold: bool, italic: bool) -> Font {
    Font {
        weight: if bold { FontWeight::BOLD } else { FontWeight::NORMAL },
        style: if italic { FontStyle::Italic } else { FontStyle::Normal },
        ..font(text::MONO_FONT)
    }
}

/// A cell's size in the code font.
pub fn cell_size(window: &Window) -> Size<Pixels> {
    let ts = window.text_system();
    let id = ts.resolve_font(&mono(false, false));
    let width = ts.advance(id, FONT_SIZE, 'm').map(|s| s.width).unwrap_or(px(7.5));
    size(width, (FONT_SIZE * 1.3).round())
}

fn hsla(rgb: u32) -> Hsla {
    gpui::rgb(rgb).into()
}

/// The grid: fills its space, reports how many cells fit (`on_layout`, every layout), and
/// paints `frame`.
pub fn terminal_grid(frame: Arc<Frame>, focused: bool, on_layout: LayoutHandler) -> impl IntoElement {
    canvas(
        move |bounds, window, cx| {
            let cell = cell_size(window);
            let inner = size((bounds.size.width - PAD * 2.).max(px(0.)), (bounds.size.height - PAD * 2.).max(px(0.)));
            let layout = GridLayout {
                origin: point(bounds.origin.x + PAD, bounds.origin.y + PAD),
                cell,
                cols: (f32::from(inner.width) / f32::from(cell.width)).floor().max(2.) as u16,
                rows: (f32::from(inner.height) / f32::from(cell.height)).floor().max(1.) as u16,
            };
            on_layout(layout, window, cx);
            layout
        },
        move |bounds, layout, window, cx| {
            window.with_content_mask(Some(ContentMask { bounds }), |window| paint(&frame, focused, layout, window, cx));
        },
    )
    .size_full()
}

/// What a run of cells shares to be shaped together.
#[derive(PartialEq, Clone, Copy)]
struct Style {
    fg: u32,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

fn style(c: &FrameCell) -> Style {
    Style { fg: c.fg, bold: c.flags.bold, italic: c.flags.italic, underline: c.flags.underline, strike: c.flags.strike }
}

fn paint(frame: &Frame, focused: bool, layout: GridLayout, window: &mut Window, cx: &mut App) {
    let GridLayout { origin, cell, .. } = layout;
    let at = |col: usize, row: usize| point(origin.x + cell.width * col as f32, origin.y + cell.height * row as f32);
    let span = |from: usize, to: usize, row: usize| Bounds::new(at(from, row), size(cell.width * (to - from) as f32, cell.height));
    let text_line = |s: String, st: Style, color: u32, window: &mut Window| {
        let run = TextRun {
            len: s.len(),
            font: mono(st.bold, st.italic),
            color: hsla(color),
            background_color: None,
            underline: st.underline.then(|| UnderlineStyle { thickness: px(1.), color: None, wavy: false }),
            strikethrough: st.strike.then(|| StrikethroughStyle { thickness: px(1.), color: None }),
        };
        window.text_system().shape_line(s.into(), FONT_SIZE, &[run], None)
    };
    for (row, line) in frame.lines.iter().enumerate() {
        let cols = line.len();
        // Backgrounds (the default one isn't painted), then the selection over them.
        let mut c = 0;
        while c < cols {
            match line[c].bg {
                Some(bg) => {
                    let start = c;
                    while c < cols && line[c].bg == Some(bg) {
                        c += 1;
                    }
                    window.paint_quad(fill(span(start, c, row), hsla(bg)));
                }
                None => c += 1,
            }
        }
        let mut c = 0;
        while c < cols {
            if line[c].flags.selected {
                let start = c;
                while c < cols && line[c].flags.selected {
                    c += 1;
                }
                window.paint_quad(fill(span(start, c, row), color::selection()));
            } else {
                c += 1;
            }
        }
        // Text, in runs of one style. A wide character is its own run at its own cell, so a
        // fallback font's width can't push the rest of the line out of its columns.
        let mut c = 0;
        while c < cols {
            let first = &line[c];
            if first.flags.spacer {
                c += 1;
                continue;
            }
            let st = style(first);
            let start = c;
            let mut s = String::new();
            if first.flags.wide {
                s.push(first.ch);
                s.extend(first.marks.iter().flat_map(|m| m.chars()));
                c += 1;
            } else {
                while c < cols {
                    let x = &line[c];
                    if x.flags.spacer || x.flags.wide || style(x) != st {
                        break;
                    }
                    s.push(x.ch);
                    s.extend(x.marks.iter().flat_map(|m| m.chars()));
                    c += 1;
                }
            }
            if s.trim().is_empty() && !st.underline && !st.strike {
                continue;
            }
            let _ = text_line(s, st, st.fg, window).paint(at(start, row), cell.height, TextAlign::Left, None, window, cx);
        }
    }
    let Some(cur) = frame.cursor else { return };
    let width = if cur.wide { cell.width * 2. } else { cell.width };
    let origin = at(cur.col as usize, cur.row as usize);
    let cursor = hsla(frame.palette.cursor);
    match cur.shape {
        CursorShape::Block if focused => {
            window.paint_quad(fill(Bounds::new(origin, size(width, cell.height)), cursor));
            if !cur.ch.is_whitespace() {
                let under = &frame.lines[cur.row as usize][cur.col as usize];
                let line = text_line(cur.ch.to_string(), style(under), frame.palette.background, window);
                let _ = line.paint(origin, cell.height, TextAlign::Left, None, window, cx);
            }
        }
        CursorShape::Block => window.paint_quad(outline(Bounds::new(origin, size(width, cell.height)), cursor, BorderStyle::Solid)),
        CursorShape::Beam => window.paint_quad(fill(Bounds::new(origin, size(px(2.), cell.height)), cursor)),
        CursorShape::Underline => {
            window.paint_quad(fill(Bounds::new(point(origin.x, origin.y + cell.height - px(2.)), size(width, px(2.))), cursor))
        }
    }
}

/// One tab in the strip.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalTab {
    pub title: SharedString,
    /// Output arrived while it wasn't shown.
    pub unread: bool,
    /// Its shell ended.
    pub ended: bool,
}

pub type IndexHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub struct StripHandlers {
    pub on_select: IndexHandler,
    pub on_close: IndexHandler,
    pub on_new: Handler,
    pub on_maximize: Handler,
    pub on_hide: Handler,
}

impl Default for StripHandlers {
    fn default() -> Self {
        let noop: Handler = Rc::new(|_, _| {});
        Self { on_select: Rc::new(|_, _, _| {}), on_close: Rc::new(|_, _, _| {}), on_new: noop.clone(), on_maximize: noop.clone(), on_hide: noop }
    }
}

/// The pane's header: a tab per terminal, then new, maximize and hide.
pub fn tab_strip(tabs: &[TerminalTab], selected: usize, maximized: bool, h: &StripHandlers) -> Div {
    let tab = |ix: usize, t: &TerminalTab| {
        let on = ix == selected;
        let group: SharedString = format!("terminal-tab-{ix}").into();
        let (select, close) = (h.on_select.clone(), h.on_close.clone());
        div()
            .id(("terminal-tab", ix))
            .group(group.clone())
            .flex_none()
            .max_w(px(180.))
            .h(px(24.))
            .pl_2()
            .pr_1()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded(radius::MD)
            .text_size(text::SM)
            .cursor_pointer()
            .map(|d| if on { d.bg(color::hover()).text_color(color::text()) } else { d.text_color(color::text_3()) })
            .hover(|s| s.bg(color::hover()).text_color(color::text()))
            .child(Icon::new(Lucide::SquareTerminal).size(size::ICON).text_color(if t.ended { color::text_3() } else { color::text_2() }))
            .child(div().min_w_0().truncate().when(t.ended, |d| d.text_color(color::text_3())).child(t.title.clone()))
            .when(t.unread && !on, |d| d.child(div().size(px(6.)).flex_none().rounded_full().bg(color::blue())))
            .child(
                div()
                    .id(("terminal-tab-close", ix))
                    .size(px(18.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(radius::SM)
                    .text_color(color::text_3())
                    .when(!on, |d| d.invisible().group_hover(group, |s| s.visible()))
                    .hover(|s| s.bg(color::pressed()).text_color(color::text()))
                    .child(Icon::new(Lucide::X).size(size::ICON_SM))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, w, cx| {
                        cx.stop_propagation();
                        close(ix, w, cx)
                    }),
            )
            .on_click(move |_, w, cx| select(ix, w, cx))
    };
    let (new, maximize, hide) = (h.on_new.clone(), h.on_maximize.clone(), h.on_hide.clone());
    div()
        .h(STRIP_H)
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .border_b_1()
        .border_color(color::border())
        .child(div().id("terminal-tabs").flex_1().min_w_0().flex().items_center().gap_1().overflow_x_scroll().children(
            tabs.iter().enumerate().map(|(ix, t)| tab(ix, t)),
        ))
        .child(icon_button("terminal-new", Lucide::Plus, true, false).tip_action("New terminal", &NewTerminal).on_click(move |_, w, cx| new(w, cx)))
        .child(
            icon_button("terminal-maximize", if maximized { Lucide::Minimize2 } else { Lucide::Maximize2 }, true, false)
                .tip_action(if maximized { "Restore" } else { "Maximize" }, &MaximizeTerminal)
                .on_click(move |_, w, cx| maximize(w, cx)),
        )
        .child(icon_button("terminal-hide", Lucide::X, true, false).tip_action("Hide terminal", &ToggleTerminal).on_click(move |_, w, cx| hide(w, cx)))
}

/// Under a terminal whose shell ended.
pub fn ended_notice(exit_code: Option<i32>) -> Div {
    let how = match exit_code {
        Some(0) | None => "Process exited".to_string(),
        Some(code) => format!("Process exited with code {code}"),
    };
    div()
        .flex_none()
        .h(px(32.))
        .px(PAD)
        .flex()
        .items_center()
        .gap_2()
        .border_t_1()
        .border_color(color::border())
        .text_size(text::SM)
        .text_color(color::text_3())
        .child(how)
        .child("·")
        .child("Press Enter for a new shell")
}

/// In place of a terminal that can't be shown (its plugin stopped, or it couldn't start).
pub fn stopped_notice(message: impl Into<SharedString>, on_restart: Handler) -> Div {
    div().flex_1().flex().flex_col().items_center().justify_center().gap_3().px_6().child(
        div().max_w(px(360.)).text_center().text_size(text::SM).text_color(color::text_2()).child(message.into()),
    )
    .child(pill_button("terminal-restart", "New terminal", false).on_click(move |_, w, cx| on_restart(w, cx)))
}

#[cfg(feature = "dev")]
pub fn sample_frame(cols: u16, rows: u16) -> Frame {
    use space_terminal::{Screen, Size};
    let mut screen = Screen::new(Size { cols, rows, cell_width: 7.5, cell_height: 16. }, crate::theme::terminal_palette());
    let mut text = String::from("\x1b[1;34m~/projects/space\x1b[0m \x1b[32mmain\x1b[0m ❯ cargo test -p space-core\r\n");
    text.push_str("   \x1b[1;32mCompiling\x1b[0m space-core v0.1.0\r\n");
    text.push_str("    \x1b[1;32mFinished\x1b[0m test profile in 3.2s\r\ntest session::tests::reducer ... \x1b[32mok\x1b[0m\r\n");
    text.push_str("test result: \x1b[32mok\x1b[0m. 28 passed; 0 failed\r\n\r\n");
    for (i, name) in ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"].iter().enumerate() {
        text.push_str(&format!("\x1b[3{i}m{name:<8}\x1b[0m"));
    }
    text.push_str("\r\n");
    for (i, name) in ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"].iter().enumerate() {
        text.push_str(&format!("\x1b[9{i}m{name:<8}\x1b[0m"));
    }
    text.push_str("\r\n\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m \x1b[9mstrike\x1b[0m \x1b[7m inverse \x1b[0m 漢字\r\n\r\n");
    text.push_str("\x1b[1;34m~/projects/space\x1b[0m \x1b[32mmain\x1b[0m ❯ ");
    screen.feed(text.as_bytes());
    screen.frame()
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    let tabs = vec![
        TerminalTab { title: "zsh".into(), unread: false, ended: false },
        TerminalTab { title: "cargo".into(), unread: true, ended: false },
        TerminalTab { title: "zsh".into(), unread: false, ended: true },
    ];
    let t2 = tabs.clone();
    vec![ComponentPreview::new(Area::Shell, "Terminal", "The terminal pane: tabs, the grid in the code font, and notices.").stacked().examples([
        Example::new("pane", move |_, _| {
            div()
                .w(px(640.))
                .h(px(340.))
                .flex()
                .flex_col()
                .bg(color::bg())
                .border_1()
                .border_color(color::border())
                .child(tab_strip(&tabs, 0, false, &StripHandlers::default()))
                .child(div().flex_1().child(terminal_grid(Arc::new(sample_frame(80, 14)), true, Rc::new(|_, _, _| {}))))
                .into_any_element()
        }),
        Example::new("ended", move |_, _| {
            div()
                .w(px(640.))
                .flex()
                .flex_col()
                .bg(color::bg())
                .border_1()
                .border_color(color::border())
                .child(tab_strip(&t2, 2, true, &StripHandlers::default()))
                .child(div().h(px(80.)))
                .child(ended_notice(Some(130)))
                .into_any_element()
        }),
        Example::new("stopped", |_, _| {
            div()
                .w(px(640.))
                .h(px(160.))
                .flex()
                .bg(color::bg())
                .border_1()
                .border_color(color::border())
                .child(stopped_notice("The terminal plugin stopped, and this terminal's shell with it.", Rc::new(|_, _| {})))
                .into_any_element()
        }),
    ])]
}
