//! Dark palette and fonts.

use gpui::{Hsla, rgb};

pub const UI_FONT: &str = ".SystemUIFont";
pub const MONO_FONT: &str = "Menlo";

pub fn bg() -> Hsla {
    rgb(0x16171b).into()
}
pub fn sidebar() -> Hsla {
    rgb(0x1c1d22).into()
}
pub fn panel() -> Hsla {
    rgb(0x22242a).into()
}
pub fn panel_hover() -> Hsla {
    rgb(0x2a2c33).into()
}
pub fn border() -> Hsla {
    rgb(0x2f3138).into()
}
pub fn text() -> Hsla {
    rgb(0xe4e5e9).into()
}
pub fn muted() -> Hsla {
    rgb(0x8b8e98).into()
}
pub fn faint() -> Hsla {
    rgb(0x5d606a).into()
}
pub fn accent() -> Hsla {
    rgb(0x7aa2f7).into()
}
pub fn user_bubble() -> Hsla {
    rgb(0x263452).into()
}
pub fn green() -> Hsla {
    rgb(0x73c991).into()
}
pub fn yellow() -> Hsla {
    rgb(0xe0b85c).into()
}
pub fn red() -> Hsla {
    rgb(0xe06c75).into()
}
