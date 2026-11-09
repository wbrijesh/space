//! Space's terminal screen: the app side of a terminal. The terminal plugin runs the shell and
//! sends its bytes; a [`Screen`] parses them with alacritty's terminal state (grid, scrollback,
//! modes), turns keys, pastes and mouse events into the bytes the shell expects, keeps the
//! selection, and hands the UI a plain [`Frame`] to draw (no alacritty types, colors resolved
//! to RGB with Space's [`Palette`]).

mod keys;
mod screen;

pub use keys::{Key, Mods, key_bytes, paste_bytes};
pub use screen::{CellFlags, CursorShape, Frame, FrameCell, FrameCursor, MouseButton, MouseEvent, Palette, Screen, SelectKind, Size};
