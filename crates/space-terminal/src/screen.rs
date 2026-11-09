//! One terminal's screen: alacritty's `Term` fed by the shell's bytes.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{self, Color, NamedColor, Processor, Rgb};

use crate::keys::{Key, Mods, key_bytes};

/// Lines kept above the screen.
const SCROLLBACK: usize = 10_000;

/// Columns and rows, and a cell's size in pixels (programs may ask for it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: f32,
    pub cell_height: f32,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

/// The terminal's colors (`0xRRGGBB`): the 16 ANSI colors, then the default foreground,
/// background and cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub ansi: [u32; 16],
    pub foreground: u32,
    pub background: u32,
    pub cursor: u32,
}

impl Palette {
    /// One of the 256 indexed colors: the 16 ANSI ones, the 6×6×6 cube, then 24 grays.
    pub fn indexed(&self, i: u8) -> u32 {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                let i = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v as u32 };
                (level(i / 36) << 16) | (level((i / 6) % 6) << 8) | level(i % 6)
            }
            _ => {
                let g = 8 + 10 * (i as u32 - 232);
                (g << 16) | (g << 8) | g
            }
        }
    }
}

fn pack(c: Rgb) -> u32 {
    (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32
}

fn unpack(c: u32) -> Rgb {
    Rgb { r: (c >> 16) as u8, g: (c >> 8) as u8, b: c as u8 }
}

/// `a` moved toward `b` by `t` (0–1).
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xff) as f32, ((b >> shift) & 0xff) as f32);
        ((x + (y - x) * t).round() as u32) << shift
    };
    ch(16) | ch(8) | ch(0)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CellFlags {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// The first of a two-column character.
    pub wide: bool,
    /// The second column of a wide character (draw nothing).
    pub spacer: bool,
    pub selected: bool,
}

/// One cell, colors resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameCell {
    pub ch: char,
    /// Combining marks drawn on the character.
    pub marks: Option<String>,
    pub fg: u32,
    /// `None`: the default background (not painted).
    pub bg: Option<u32>,
    pub flags: CellFlags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameCursor {
    pub col: u16,
    pub row: u16,
    pub shape: CursorShape,
    /// The character under it (drawn in the background color on a block).
    pub ch: char,
    pub wide: bool,
}

/// What the screen shows now.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    pub lines: Vec<Vec<FrameCell>>,
    /// `None` when hidden or scrolled out of view.
    pub cursor: Option<FrameCursor>,
    /// Lines scrolled back from the bottom.
    pub scrolled_back: usize,
    pub palette: Palette,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKind {
    /// Character by character (a drag).
    Simple,
    /// Whole words (a double-click).
    Word,
    /// Whole lines (a triple-click).
    Line,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// Moving with no button down.
    None,
}

/// A mouse event for a program that asked for them (vim, htop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub button: MouseButton,
    pub pressed: bool,
    pub motion: bool,
    pub col: u16,
    pub row: u16,
    pub mods: Mods,
}

#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.lock().unwrap().push(event);
    }
}

pub struct Screen {
    term: Term<Listener>,
    parser: Processor,
    events: Arc<Mutex<Vec<Event>>>,
    palette: Palette,
    size: Size,
    title: Option<String>,
}

impl Screen {
    pub fn new(size: Size, palette: Palette) -> Self {
        let listener = Listener::default();
        let events = listener.0.clone();
        let config = Config { scrolling_history: SCROLLBACK, ..Config::default() };
        let size = clamp(size);
        Self { term: Term::new(config, &size, listener), parser: Processor::new(), events, palette, size, title: None }
    }

    /// Parse the shell's output. Returns what the terminal answers (cursor position reports,
    /// color queries...), to write back to the shell.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.parser.advance(&mut self.term, bytes);
        let events: Vec<Event> = std::mem::take(&mut *self.events.lock().unwrap());
        let mut reply = Vec::new();
        for event in events {
            match event {
                Event::PtyWrite(text) => reply.extend_from_slice(text.as_bytes()),
                Event::ColorRequest(index, format) => reply.extend_from_slice(format(unpack(self.color_at(index))).as_bytes()),
                Event::TextAreaSizeRequest(format) => {
                    let window = WindowSize {
                        num_lines: self.size.rows,
                        num_cols: self.size.cols,
                        cell_width: self.size.cell_width as u16,
                        cell_height: self.size.cell_height as u16,
                    };
                    reply.extend_from_slice(format(window).as_bytes());
                }
                Event::Title(title) => self.title = Some(title),
                Event::ResetTitle => self.title = None,
                _ => {}
            }
        }
        reply
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn resize(&mut self, size: Size) {
        let size = clamp(size);
        if (size.cols, size.rows) != (self.size.cols, self.size.rows) {
            self.term.resize(size);
        }
        self.size = size;
    }

    /// The title the program set (OSC 0/2), if any.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// The program asked for application cursor keys.
    pub fn app_cursor(&self) -> bool {
        self.mode().contains(TermMode::APP_CURSOR)
    }

    pub fn bracketed_paste(&self) -> bool {
        self.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// The program takes mouse events (clicks go to it unless ⌥ is held).
    pub fn mouse_mode(&self) -> bool {
        self.mode().intersects(TermMode::MOUSE_MODE)
    }

    pub fn alt_screen(&self) -> bool {
        self.mode().contains(TermMode::ALT_SCREEN)
    }

    /// The bytes for a key, with the program's cursor-key mode.
    pub fn key(&self, key: &Key, mods: Mods) -> Option<Vec<u8>> {
        key_bytes(key, mods, self.app_cursor())
    }

    /// Focus moved to (or away from) the view: the cursor's look, and a report to programs
    /// that asked for one.
    pub fn set_focused(&mut self, focused: bool) -> Option<Vec<u8>> {
        self.term.is_focused = focused;
        self.mode().contains(TermMode::FOCUS_IN_OUT).then(|| if focused { b"\x1b[I".to_vec() } else { b"\x1b[O".to_vec() })
    }

    pub fn scrolled_back(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Scroll the view through the scrollback (positive: up, toward older lines).
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// A wheel turned `lines` (positive: up) over a cell. A program that takes the mouse gets
    /// wheel events; a full-screen one without mouse gets arrow keys; otherwise the view
    /// scrolls. Returns bytes for the shell, if any.
    pub fn wheel(&mut self, lines: i32, col: u16, row: u16, mods: Mods) -> Option<Vec<u8>> {
        if lines == 0 {
            return None;
        }
        let mode = self.mode();
        if mode.intersects(TermMode::MOUSE_MODE) {
            let code = if lines > 0 { 64 } else { 65 };
            let one = self.mouse_bytes(code, true, col, row, mods);
            return Some(one.repeat(lines.unsigned_abs().min(10) as usize));
        }
        if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            let key = if lines > 0 { Key::Up } else { Key::Down };
            let one = key_bytes(&key, Mods::default(), self.app_cursor()).unwrap_or_default();
            return Some(one.repeat(lines.unsigned_abs().min(10) as usize));
        }
        self.scroll(lines);
        None
    }

    /// A mouse event for a program that takes the mouse (`None` when it doesn't want this one).
    pub fn mouse(&self, event: MouseEvent) -> Option<Vec<u8>> {
        let mode = self.mode();
        if !mode.intersects(TermMode::MOUSE_MODE) {
            return None;
        }
        if event.motion {
            let dragging = event.button != MouseButton::None;
            if !(mode.contains(TermMode::MOUSE_MOTION) || (dragging && mode.contains(TermMode::MOUSE_DRAG))) {
                return None;
            }
        }
        let base = match event.button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::None => 3,
        };
        let code = base + if event.motion { 32 } else { 0 };
        Some(self.mouse_bytes(code, event.pressed || event.motion, event.col, event.row, event.mods))
    }

    fn mouse_bytes(&self, code: u8, pressed: bool, col: u16, row: u16, mods: Mods) -> Vec<u8> {
        let code = code + if mods.shift { 4 } else { 0 } + if mods.alt { 8 } else { 0 } + if mods.ctrl { 16 } else { 0 };
        if self.mode().contains(TermMode::SGR_MOUSE) {
            format!("\x1b[<{code};{};{}{}", col + 1, row + 1, if pressed { 'M' } else { 'm' }).into_bytes()
        } else {
            // X10/normal: a release is button 3; coordinates past 222 can't be sent.
            let code = if pressed || code >= 64 { code } else { 3 + (code & !3) };
            let at = |v: u16| (32 + 1 + v.min(222)) as u8;
            vec![0x1b, b'[', b'M', 32 + code, at(col), at(row)]
        }
    }

    fn point(&self, col: u16, row: u16) -> Point {
        let offset = self.term.grid().display_offset() as i32;
        let col = (col as usize).min(self.size.cols.saturating_sub(1) as usize);
        Point::new(Line(row as i32 - offset), Column(col))
    }

    /// Start a selection at a cell (`right`: the pointer is in the cell's right half).
    pub fn select(&mut self, kind: SelectKind, col: u16, row: u16, right: bool) {
        let ty = match kind {
            SelectKind::Simple => SelectionType::Simple,
            SelectKind::Word => SelectionType::Semantic,
            SelectKind::Line => SelectionType::Lines,
        };
        let side = if right { Direction::Right } else { Direction::Left };
        self.term.selection = Some(Selection::new(ty, self.point(col, row), side));
    }

    /// Move the selection's end.
    pub fn extend_selection(&mut self, col: u16, row: u16, right: bool) {
        let point = self.point(col, row);
        let side = if right { Direction::Right } else { Direction::Left };
        if let Some(sel) = self.term.selection.as_mut() {
            sel.update(point, side);
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// The selected text (`None` when nothing, or only empty cells, is selected).
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    /// Forget the scrollback (⌘K; the shell is then told to redraw with ^L).
    pub fn clear_scrollback(&mut self) {
        self.term.selection = None;
        self.term.grid_mut().clear_history();
        self.term.scroll_display(Scroll::Bottom);
    }

    /// A link under a cell: an `http(s)://` or `file://` URL.
    pub fn url_at(&self, col: u16, row: u16) -> Option<String> {
        let line = self.point(0, row).line;
        let grid = self.term.grid();
        let cells: Vec<(usize, char)> = (0..self.size.cols as usize)
            .filter_map(|c| {
                let cell = &grid[line][Column(c)];
                (!cell.flags.contains(Flags::WIDE_CHAR_SPACER)).then_some((c, cell.c))
            })
            .collect();
        let text: String = cells.iter().map(|(_, c)| *c).collect();
        let chars: Vec<char> = text.chars().collect();
        let at = cells.iter().position(|(c, _)| *c >= col as usize)?;
        let mut start = 0;
        while let Some(found) = find_scheme(&chars, start) {
            let mut end = found;
            while end < chars.len() && !chars[end].is_whitespace() && !"<>\"'`".contains(chars[end]) {
                end += 1;
            }
            // Trailing punctuation belongs to the sentence (a `)` only if it isn't balanced).
            while end > found {
                let last = chars[end - 1];
                let opens = chars[found..end].iter().filter(|c| **c == '(').count();
                let closes = chars[found..end].iter().filter(|c| **c == ')').count();
                if ".,;:!?".contains(last) || (last == ')' && closes > opens) {
                    end -= 1;
                } else {
                    break;
                }
            }
            if (found..end).contains(&at) {
                let url: String = chars[found..end].iter().collect();
                return (url.len() > "https://".len()).then_some(url);
            }
            start = end.max(found + 1);
        }
        None
    }

    fn color_at(&self, index: usize) -> u32 {
        if let Some(c) = self.term.colors()[index] {
            return pack(c);
        }
        match index {
            0..=255 => self.palette.indexed(index as u8),
            i if i == NamedColor::Background as usize => self.palette.background,
            i if i == NamedColor::Cursor as usize => self.palette.cursor,
            _ => self.palette.foreground,
        }
    }

    fn resolve(&self, color: Color, colors: &Colors) -> u32 {
        match color {
            Color::Spec(rgb) => pack(rgb),
            Color::Indexed(i) => colors[i as usize].map(pack).unwrap_or_else(|| self.palette.indexed(i)),
            Color::Named(named) => {
                let index = named as usize;
                if let Some(c) = colors[index] {
                    return pack(c);
                }
                match named {
                    NamedColor::Foreground | NamedColor::BrightForeground => self.palette.foreground,
                    NamedColor::Background => self.palette.background,
                    NamedColor::Cursor => self.palette.cursor,
                    NamedColor::DimForeground => mix(self.palette.foreground, self.palette.background, 0.34),
                    n if (n as usize) < 16 => self.palette.ansi[n as usize],
                    // DimBlack..DimWhite.
                    n => {
                        let base = (n as usize).saturating_sub(NamedColor::DimBlack as usize).min(7);
                        mix(self.palette.ansi[base], self.palette.background, 0.34)
                    }
                }
            }
        }
    }

    /// What to draw now.
    pub fn frame(&self) -> Frame {
        let content = self.term.renderable_content();
        let (cols, rows) = (self.size.cols, self.size.rows);
        let offset = content.display_offset as i32;
        let blank = FrameCell { ch: ' ', marks: None, fg: self.palette.foreground, bg: None, flags: CellFlags::default() };
        let mut lines = vec![vec![blank; cols as usize]; rows as usize];
        let selection = content.selection;
        let colors = content.colors;
        for indexed in content.display_iter {
            let row = indexed.point.line.0 + offset;
            let col = indexed.point.column.0;
            if row < 0 || row >= rows as i32 || col >= cols as usize {
                continue;
            }
            let cell = indexed.cell;
            let f = cell.flags;
            let mut fg = self.resolve(cell.fg, colors);
            let mut bg = match cell.bg {
                Color::Named(NamedColor::Background) if colors[NamedColor::Background as usize].is_none() => None,
                other => Some(self.resolve(other, colors)),
            };
            if f.contains(Flags::DIM) {
                fg = mix(fg, bg.unwrap_or(self.palette.background), 0.34);
            }
            if f.contains(Flags::INVERSE) {
                let old_bg = bg.unwrap_or(self.palette.background);
                bg = Some(fg);
                fg = old_bg;
            }
            if f.contains(Flags::HIDDEN) {
                fg = bg.unwrap_or(self.palette.background);
            }
            lines[row as usize][col] = FrameCell {
                ch: if cell.c == '\0' { ' ' } else { cell.c },
                marks: cell.zerowidth().map(|z| z.iter().collect()),
                fg,
                bg,
                flags: CellFlags {
                    bold: f.contains(Flags::BOLD),
                    italic: f.contains(Flags::ITALIC),
                    underline: f.intersects(Flags::ALL_UNDERLINES),
                    strike: f.contains(Flags::STRIKEOUT),
                    wide: f.contains(Flags::WIDE_CHAR),
                    spacer: f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
                    selected: selection.is_some_and(|s| s.contains(indexed.point)),
                },
            };
        }
        let cursor = {
            let c = content.cursor;
            let row = c.point.line.0 + offset;
            let shape = match c.shape {
                ansi::CursorShape::Hidden => None,
                ansi::CursorShape::Beam => Some(CursorShape::Beam),
                ansi::CursorShape::Underline => Some(CursorShape::Underline),
                ansi::CursorShape::Block | ansi::CursorShape::HollowBlock => Some(CursorShape::Block),
            };
            match shape {
                Some(shape) if (0..rows as i32).contains(&row) && c.point.column.0 < cols as usize => {
                    let cell = &lines[row as usize][c.point.column.0];
                    Some(FrameCursor { col: c.point.column.0 as u16, row: row as u16, shape, ch: cell.ch, wide: cell.flags.wide })
                }
                _ => None,
            }
        };
        Frame { cols, rows, lines, cursor, scrolled_back: content.display_offset, palette: self.palette }
    }
}

fn clamp(size: Size) -> Size {
    Size { cols: size.cols.max(2), rows: size.rows.max(1), ..size }
}

/// The next `http://`, `https://` or `file://` at or after `from`.
fn find_scheme(chars: &[char], from: usize) -> Option<usize> {
    let schemes: [&[char]; 3] = [&['h', 't', 't', 'p', 's', ':', '/', '/'], &['h', 't', 't', 'p', ':', '/', '/'], &['f', 'i', 'l', 'e', ':', '/', '/']];
    (from..chars.len()).find(|&i| schemes.iter().any(|s| chars[i..].starts_with(s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PALETTE: Palette = Palette {
        ansi: [
            0x484f58, 0xff7b72, 0x3fb950, 0xd29922, 0x58a6ff, 0xbc8cff, 0x39c5cf, 0xb1bac4, 0x6e7681, 0xffa198, 0x56d364, 0xe3b341, 0x79c0ff,
            0xd2a8ff, 0x56d4dd, 0xffffff,
        ],
        foreground: 0xececec,
        background: 0x141414,
        cursor: 0xececec,
    };

    fn screen(cols: u16, rows: u16) -> Screen {
        Screen::new(Size { cols, rows, cell_width: 8., cell_height: 16. }, PALETTE)
    }

    fn row_text(f: &Frame, row: usize) -> String {
        f.lines[row].iter().filter(|c| !c.flags.spacer).map(|c| c.ch).collect::<String>().trim_end().to_string()
    }

    #[test]
    fn text_colors_and_attributes() {
        let mut s = screen(20, 3);
        s.feed(b"plain \x1b[31mred\x1b[0m \x1b[1;42mbold\x1b[0m\r\n\x1b[38;5;196mx\x1b[38;2;1;2;3my\x1b[7mz");
        let f = s.frame();
        assert_eq!(row_text(&f, 0), "plain red bold");
        assert_eq!(f.lines[0][0].fg, PALETTE.foreground);
        assert_eq!(f.lines[0][0].bg, None, "the default background isn't painted");
        assert_eq!(f.lines[0][6].fg, PALETTE.ansi[1]);
        assert!(f.lines[0][10].flags.bold);
        assert_eq!(f.lines[0][10].bg, Some(PALETTE.ansi[2]));
        assert_eq!(f.lines[1][0].fg, 0xff0000, "index 196 is pure red in the cube");
        assert_eq!(f.lines[1][1].fg, 0x010203);
        // Inverse: the default colors swap.
        assert_eq!((f.lines[1][2].fg, f.lines[1][2].bg), (PALETTE.background, Some(0x010203)));
        assert_eq!(f.cursor.map(|c| (c.col, c.row)), Some((3, 1)));
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let mut s = screen(10, 2);
        s.feed("a漢b".as_bytes());
        let f = s.frame();
        assert!(f.lines[0][1].flags.wide);
        assert!(f.lines[0][2].flags.spacer);
        assert_eq!(f.lines[0][3].ch, 'b');
        assert_eq!(row_text(&f, 0), "a漢b");
    }

    #[test]
    fn answers_queries_and_keeps_titles() {
        let mut s = screen(20, 5);
        assert_eq!(s.feed(b"ab\x1b[6n"), b"\x1b[1;3R", "cursor position report");
        let reply = String::from_utf8(s.feed(b"\x1b]11;?\x07")).unwrap();
        assert!(reply.contains("rgb:1414/1414/1414"), "background query: {reply}");
        s.feed(b"\x1b]0;vim notes.md\x07");
        assert_eq!(s.title(), Some("vim notes.md"));
    }

    #[test]
    fn scrollback_selection_and_clearing() {
        let mut s = screen(10, 3);
        for i in 0..10 {
            s.feed(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(row_text(&s.frame(), 0), "line 8");
        s.scroll(3);
        assert_eq!((s.scrolled_back(), row_text(&s.frame(), 0).as_str()), (3, "line 5"));
        s.select(SelectKind::Line, 0, 0, false);
        s.extend_selection(0, 1, false);
        assert_eq!(s.selection_text().as_deref(), Some("line 5\nline 6\n"));
        assert!(s.frame().lines[0][0].flags.selected);
        s.select(SelectKind::Word, 1, 0, false);
        assert_eq!(s.selection_text().as_deref(), Some("line"));
        s.clear_scrollback();
        assert_eq!(s.scrolled_back(), 0);
        s.scroll(5);
        assert_eq!(s.scrolled_back(), 0, "nothing left to scroll to");
    }

    #[test]
    fn resizing_keeps_the_text() {
        let mut s = screen(10, 3);
        s.feed(b"hello");
        s.resize(Size { cols: 30, rows: 6, cell_width: 8., cell_height: 16. });
        let f = s.frame();
        assert_eq!((f.cols, f.rows, f.lines.len(), f.lines[0].len()), (30, 6, 6, 30));
        assert_eq!(row_text(&f, 0), "hello");
    }

    #[test]
    fn modes_programs_switch() {
        let mut s = screen(20, 5);
        assert!(!s.app_cursor() && !s.bracketed_paste() && !s.mouse_mode() && !s.alt_screen());
        s.feed(b"\x1b[?1h\x1b[?2004h\x1b[?1049h\x1b[?1000h\x1b[?1006h");
        assert!(s.app_cursor() && s.bracketed_paste() && s.mouse_mode() && s.alt_screen());
        assert_eq!(s.key(&Key::Up, Mods::default()).unwrap(), b"\x1bOA");
        let click = MouseEvent { button: MouseButton::Left, pressed: true, motion: false, col: 4, row: 2, mods: Mods::default() };
        assert_eq!(s.mouse(click).unwrap(), b"\x1b[<0;5;3M");
        assert_eq!(s.mouse(MouseEvent { pressed: false, ..click }).unwrap(), b"\x1b[<0;5;3m");
        assert_eq!(s.mouse(MouseEvent { motion: true, ..click }), None, "no motion without asking");
        assert_eq!(s.wheel(1, 0, 0, Mods::default()).unwrap(), b"\x1b[<64;1;1M");
        // Without mouse reporting a full-screen program gets arrows for the wheel...
        s.feed(b"\x1b[?1000l\x1b[?1007h");
        assert_eq!(s.wheel(-2, 0, 0, Mods::default()).unwrap(), b"\x1bOB\x1bOB");
        // ...and the normal screen scrolls (nothing to send).
        s.feed(b"\x1b[?1049l");
        assert_eq!(s.wheel(1, 0, 0, Mods::default()), None);
        assert_eq!(s.set_focused(true), None);
        s.feed(b"\x1b[?1004h");
        assert_eq!(s.set_focused(false).unwrap(), b"\x1b[O");
    }

    #[test]
    fn links_under_the_pointer() {
        let mut s = screen(60, 2);
        s.feed(b"see https://example.com/a_(b) and (http://x.io/p).");
        assert_eq!(s.url_at(6, 0).as_deref(), Some("https://example.com/a_(b)"));
        assert_eq!(s.url_at(40, 0).as_deref(), Some("http://x.io/p"));
        assert_eq!(s.url_at(1, 0), None);
    }

    #[test]
    fn palette_indexes() {
        assert_eq!(PALETTE.indexed(1), PALETTE.ansi[1]);
        assert_eq!(PALETTE.indexed(16), 0x000000);
        assert_eq!(PALETTE.indexed(231), 0xffffff);
        assert_eq!(PALETTE.indexed(232), 0x080808);
        assert_eq!(PALETTE.indexed(255), 0xeeeeee);
    }
}
