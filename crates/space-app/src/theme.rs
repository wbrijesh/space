//! Design tokens: one dark palette, a 4/8 px spacing scale, two font families.
//!
//! Every color in the app comes from here. `install` also maps the tokens onto the
//! gpui-component theme so stock widgets (textarea, popup menus, tooltips, scrollbars)
//! match the custom chrome.

use gpui::component::Theme;
use gpui::{App, Hsla, Pixels, px, rgb, rgba};

pub const UI_FONT: &str = ".SystemUIFont";
pub const MONO_FONT: &str = "Menlo";

// Layout.
pub const TITLE_BAR_H: Pixels = px(48.);
pub const SIDEBAR_W: Pixels = px(316.);
/// Width reserved for the macOS traffic lights (at x=18, ~52px wide) plus a gap before
/// the first title-bar button.
pub const TRAFFIC_LIGHTS_W: Pixels = px(92.);
pub const CONTENT_MAX_W: Pixels = px(760.);

// Type scale.
pub const TEXT_XS: Pixels = px(11.);
pub const TEXT_SM: Pixels = px(12.5);
pub const TEXT_MD: Pixels = px(13.5);
pub const TEXT_BODY: Pixels = px(14.5);

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// Window / main column background.
pub fn bg() -> Hsla {
    c(0x141414)
}
/// Sidebar and secondary panels.
pub fn bg_sidebar() -> Hsla {
    c(0x1a1a1a)
}
/// Raised surfaces: composer card, bubbles, cards.
pub fn surface() -> Hsla {
    c(0x1f1f1f)
}
pub fn surface_2() -> Hsla {
    c(0x262626)
}
/// Hover fill for rows and ghost buttons.
pub fn hover() -> Hsla {
    c(0x262626)
}
pub fn pressed() -> Hsla {
    c(0x2e2e2e)
}
/// 1px hairlines.
pub fn border() -> Hsla {
    c(0x2a2a2a)
}
pub fn border_strong() -> Hsla {
    c(0x3a3a3a)
}
pub fn text() -> Hsla {
    c(0xececec)
}
pub fn text_2() -> Hsla {
    c(0x9a9a9a)
}
pub fn text_3() -> Hsla {
    c(0x6b6b6b)
}
pub fn accent() -> Hsla {
    c(0x2f7cf6)
}
pub fn accent_hover() -> Hsla {
    c(0x3d86f7)
}
pub fn on_accent() -> Hsla {
    c(0xffffff)
}
pub fn green() -> Hsla {
    c(0x3fb950)
}
pub fn yellow() -> Hsla {
    c(0xd29922)
}
pub fn red() -> Hsla {
    c(0xf85149)
}
pub fn red_bg() -> Hsla {
    rgba(0xf851491a).into()
}
pub fn red_border() -> Hsla {
    rgba(0xf8514955).into()
}
pub fn yellow_border() -> Hsla {
    rgba(0xd2992266).into()
}

/// Map the tokens onto gpui-component's theme (call after `Theme::change`).
pub fn install(cx: &mut App) {
    Theme::update(cx, |t| {
        t.font_family = UI_FONT.into();
        t.mono_font_family = MONO_FONT.into();
        t.font_size = px(14.);
        t.radius = px(8.);
        t.radius_lg = px(12.);
        t.shadow = true;
        t.background = bg();
        t.foreground = text();
        t.muted = surface_2();
        t.muted_foreground = text_2();
        t.border = border();
        t.input = border_strong();
        t.ring = accent();
        t.caret = text();
        t.selection = rgba(0x2f7cf655).into();
        t.accent = hover();
        t.accent_foreground = text();
        t.popover = surface();
        t.popover_foreground = text();
        t.primary = accent();
        t.primary_hover = accent_hover();
        t.primary_active = accent();
        t.primary_foreground = on_accent();
        t.secondary = surface_2();
        t.secondary_hover = pressed();
        t.secondary_active = pressed();
        t.secondary_foreground = text();
        t.list_active = hover();
        t.list_hover = hover();
        t.link = accent_hover();
        t.scrollbar = bg();
        t.scrollbar_thumb = border_strong();
        t.scrollbar_thumb_hover = c(0x4a4a4a);
    });
}
