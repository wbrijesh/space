//! Keys and pastes to terminal bytes (xterm conventions).

/// A key the view saw. Printable keys arrive as [`Key::Char`] (the character the key types,
/// before ⌥ changes it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1–F12.
    F(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    /// ⌥, sent as Meta (an ESC before the key).
    pub alt: bool,
    /// ⌘: only the macOS editing shortcuts below; the app keeps the rest.
    pub cmd: bool,
}

impl Mods {
    /// xterm's modifier parameter (1 + shift + 2·alt + 4·ctrl), `None` without modifiers.
    fn param(self) -> Option<u8> {
        let p = 1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl);
        (p > 1).then_some(p)
    }
}

/// The bytes for a key, or `None` when the terminal doesn't take it (e.g. a ⌘ shortcut that's
/// the app's). `app_cursor`: the program asked for application cursor keys (vim, less).
pub fn key_bytes(key: &Key, mods: Mods, app_cursor: bool) -> Option<Vec<u8>> {
    let esc = |s: &str| format!("\x1b{s}").into_bytes();
    if mods.cmd {
        // The macOS line-editing shortcuts readline and zsh understand.
        return match key {
            Key::Left => Some(vec![0x01]),      // ⌘← start of line (^A)
            Key::Right => Some(vec![0x05]),     // ⌘→ end of line (^E)
            Key::Backspace => Some(vec![0x15]), // ⌘⌫ delete to start (^U)
            _ => None,
        };
    }
    let csi = |final_: char, number: Option<u8>| -> Vec<u8> {
        match (mods.param(), number) {
            (Some(m), n) => format!("\x1b[{};{m}{final_}", n.unwrap_or(1)).into_bytes(),
            (None, Some(n)) => format!("\x1b[{n}{final_}").into_bytes(),
            (None, None) => format!("\x1b[{final_}").into_bytes(),
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        match mods.param() {
            Some(m) => format!("\x1b[{n};{m}~").into_bytes(),
            None => format!("\x1b[{n}~").into_bytes(),
        }
    };
    let arrow = |final_: char| -> Vec<u8> {
        if mods.param().is_none() && app_cursor { format!("\x1bO{final_}").into_bytes() } else { csi(final_, None) }
    };
    let bytes = match key {
        Key::Char(c) => return char_bytes(*c, mods),
        Key::Enter => meta(mods, vec![b'\r']),
        Key::Tab if mods.shift => esc("[Z"),
        Key::Tab => meta(mods, vec![b'\t']),
        // ⌥⌫ deletes a word (ESC DEL), ⌃⌫ too.
        Key::Backspace if mods.alt || mods.ctrl => esc("\x7f"),
        Key::Backspace => vec![0x7f],
        Key::Escape => meta(mods, vec![0x1b]),
        // ⌥←/⌥→ move by word, as in Terminal and iTerm (ESC b / ESC f).
        Key::Left if mods.alt && !mods.ctrl && !mods.shift => esc("b"),
        Key::Right if mods.alt && !mods.ctrl && !mods.shift => esc("f"),
        Key::Up => arrow('A'),
        Key::Down => arrow('B'),
        Key::Right => arrow('C'),
        Key::Left => arrow('D'),
        Key::Home => arrow('H'),
        Key::End => arrow('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F(n @ 1..=4) => {
            let final_ = b"PQRS"[*n as usize - 1] as char;
            match mods.param() {
                Some(m) => format!("\x1b[1;{m}{final_}").into_bytes(),
                None => format!("\x1bO{final_}").into_bytes(),
            }
        }
        Key::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][*n as usize - 5]),
        Key::F(_) => return None,
    };
    Some(bytes)
}

/// ⌥ as Meta: an ESC before the key's bytes.
fn meta(mods: Mods, bytes: Vec<u8>) -> Vec<u8> {
    if mods.alt { [vec![0x1b], bytes].concat() } else { bytes }
}

fn char_bytes(c: char, mods: Mods) -> Option<Vec<u8>> {
    let mut bytes = if mods.ctrl {
        match c.to_ascii_lowercase() {
            ' ' | '@' | '2' => vec![0],
            l @ 'a'..='z' => vec![l as u8 - b'a' + 1],
            '[' | '3' => vec![0x1b],
            '\\' | '4' => vec![0x1c],
            ']' | '5' => vec![0x1d],
            '^' | '6' => vec![0x1e],
            '_' | '-' | '7' => vec![0x1f],
            '/' => vec![0x1f],
            '8' | '?' => vec![0x7f],
            _ => c.to_string().into_bytes(),
        }
    } else {
        let c = if mods.shift && c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c };
        c.to_string().into_bytes()
    };
    if mods.alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

/// A paste: line ends become CR (what Enter sends), and with bracketed paste on it's wrapped so
/// the shell doesn't run it line by line. Anything that would end the bracket early is removed.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let inner = text.replace("\x1b[201~", "");
        format!("\x1b[200~{inner}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(key: Key) -> Vec<u8> {
        key_bytes(&key, Mods::default(), false).unwrap()
    }
    fn with(key: Key, mods: Mods) -> Option<Vec<u8>> {
        key_bytes(&key, mods, false)
    }
    const CTRL: Mods = Mods { shift: false, ctrl: true, alt: false, cmd: false };
    const ALT: Mods = Mods { shift: false, ctrl: false, alt: true, cmd: false };
    const SHIFT: Mods = Mods { shift: true, ctrl: false, alt: false, cmd: false };
    const CMD: Mods = Mods { shift: false, ctrl: false, alt: false, cmd: true };

    #[test]
    fn plain_keys() {
        assert_eq!(k(Key::Char('a')), b"a");
        assert_eq!(k(Key::Char('é')), "é".as_bytes());
        assert_eq!(k(Key::Enter), b"\r");
        assert_eq!(k(Key::Tab), b"\t");
        assert_eq!(k(Key::Backspace), b"\x7f");
        assert_eq!(k(Key::Escape), b"\x1b");
        assert_eq!(k(Key::Up), b"\x1b[A");
        assert_eq!(k(Key::Delete), b"\x1b[3~");
        assert_eq!(k(Key::F(1)), b"\x1bOP");
        assert_eq!(k(Key::F(5)), b"\x1b[15~");
        assert_eq!(k(Key::F(12)), b"\x1b[24~");
    }

    #[test]
    fn control_and_meta() {
        assert_eq!(with(Key::Char('c'), CTRL).unwrap(), [3]);
        assert_eq!(with(Key::Char('D'), CTRL).unwrap(), [4]);
        assert_eq!(with(Key::Char(' '), CTRL).unwrap(), [0]);
        assert_eq!(with(Key::Char('['), CTRL).unwrap(), [0x1b]);
        assert_eq!(with(Key::Char('f'), ALT).unwrap(), b"\x1bf");
        assert_eq!(with(Key::Char('a'), SHIFT).unwrap(), b"A");
        assert_eq!(with(Key::Left, ALT).unwrap(), b"\x1bb");
        assert_eq!(with(Key::Right, ALT).unwrap(), b"\x1bf");
        assert_eq!(with(Key::Backspace, ALT).unwrap(), b"\x1b\x7f");
        assert_eq!(with(Key::Tab, SHIFT).unwrap(), b"\x1b[Z");
        assert_eq!(with(Key::Up, CTRL).unwrap(), b"\x1b[1;5A");
        assert_eq!(with(Key::Up, SHIFT).unwrap(), b"\x1b[1;2A");
        assert_eq!(with(Key::Delete, SHIFT).unwrap(), b"\x1b[3;2~");
        assert_eq!(with(Key::F(2), CTRL).unwrap(), b"\x1b[1;5Q");
    }

    #[test]
    fn application_cursor_keys() {
        assert_eq!(key_bytes(&Key::Up, Mods::default(), true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes(&Key::Home, Mods::default(), true).unwrap(), b"\x1bOH");
        // With a modifier it's the CSI form either way.
        assert_eq!(key_bytes(&Key::Up, CTRL, true).unwrap(), b"\x1b[1;5A");
    }

    #[test]
    fn command_shortcuts() {
        assert_eq!(with(Key::Left, CMD).unwrap(), [1]);
        assert_eq!(with(Key::Right, CMD).unwrap(), [5]);
        assert_eq!(with(Key::Backspace, CMD).unwrap(), [0x15]);
        assert_eq!(with(Key::Char('c'), CMD), None, "the app's");
    }

    #[test]
    fn pastes() {
        assert_eq!(paste_bytes("a\nb\r\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes("ls\n", true), b"\x1b[200~ls\r\x1b[201~");
        assert_eq!(paste_bytes("x\x1b[201~rm -rf", true), b"\x1b[200~xrm -rf\x1b[201~");
    }
}
