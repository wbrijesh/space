//! Design tokens. Every color, size, radius, font and timing in the UI comes from here;
//! components never use raw literals for these.
//!
//! * [`color`] — palette as functions (`color::text()`), plus [`color::ALL`] for the gallery.
//! * [`space`] — spacing for computed layout. Plain layout spacing uses GPUI's 4px scale
//!   helpers (`gap_2` = 8px, `px_3` = 12px), which is the same scale.
//! * [`size`] — control heights, icon sizes, layout widths.
//! * [`radius`], [`text`] (type scale + font families), [`motion`] (durations and easings).
//!
//! [`install`] maps the tokens onto gpui-component's theme so stock widgets (textarea,
//! popup menus, tooltips, scrollbars) match.

pub mod motion;

use gpui::component::Theme;
use gpui::{App, Hsla, Pixels, px, rgba};

macro_rules! colors {
    ($($(#[doc = $doc:literal])* $name:ident = $hex:literal;)*) => {
        $(
            $(#[doc = $doc])*
            pub fn $name() -> Hsla {
                rgba($hex).into()
            }
        )*
        /// Every color token as (name, `0xRRGGBBAA`, doc), for the gallery's Tokens page.
        pub const ALL: &[(&str, u32, &str)] = &[$((stringify!($name), $hex, concat!($($doc),*))),*];
    };
}

/// Color tokens (`0xRRGGBBAA`).
pub mod color {
    use super::*;

    colors! {
        /// Window / main column background.
        bg = 0x141414ff;
        /// Sidebar, code blocks.
        bg_sidebar = 0x1a1a1aff;
        /// Raised surfaces: composer card, cards, quiet hover on rows.
        surface = 0x1f1f1fff;
        /// User bubbles, muted fills.
        surface_2 = 0x262626ff;
        /// Hover fill for buttons.
        hover = 0x262626ff;
        /// Pressed / selected fill.
        pressed = 0x2e2e2eff;
        /// 1px hairlines.
        border = 0x2a2a2aff;
        /// Focused borders, tracks.
        border_strong = 0x3a3a3aff;
        /// Primary text.
        text = 0xecececff;
        /// Secondary text.
        text_2 = 0x9a9a9aff;
        /// Muted text, icons.
        text_3 = 0x6b6b6bff;
        /// Accent: selection, primary buttons, progress.
        accent = 0x2f7cf6ff;
        accent_hover = 0x3d86f7ff;
        /// Text on accent fills.
        on_accent = 0xffffffff;
        /// Text selection.
        selection = 0x2f7cf655;
        /// Status: success / done.
        green = 0x3fb950ff;
        /// Status: needs attention / waiting.
        yellow = 0xd29922ff;
        /// Status: error.
        red = 0xf85149ff;
        /// Status: new output (unread).
        blue = 0x2f7cf6ff;
        red_bg = 0xf851491a;
        red_border = 0xf8514955;
        yellow_bg = 0xd299221a;
        yellow_border = 0xd2992266;
        scrollbar_thumb_hover = 0x4a4a4aff;
    }
}

/// Spacing scale (4px base) for computed layout values.
pub mod space {
    pub const XXS: f32 = 2.;
    pub const XS: f32 = 4.;
    pub const SM: f32 = 6.;
    pub const MD: f32 = 8.;
    pub const LG: f32 = 12.;
    pub const XL: f32 = 16.;
    pub const XXL: f32 = 24.;
    /// Gallery listing.
    pub const ALL: &[(&str, f32)] = &[("XXS", XXS), ("XS", XS), ("SM", SM), ("MD", MD), ("LG", LG), ("XL", XL), ("XXL", XXL)];
}

/// Corner radii.
pub mod radius {
    use super::*;
    pub const XS: Pixels = px(4.);
    pub const SM: Pixels = px(6.);
    /// Buttons, rows.
    pub const MD: Pixels = px(7.);
    pub const LG: Pixels = px(8.);
    /// Cards, banners.
    pub const XL: Pixels = px(10.);
    pub const XXL: Pixels = px(12.);
    /// Composer card, bubbles.
    pub const CARD: Pixels = px(18.);
    pub const ALL: &[(&str, Pixels)] =
        &[("XS", XS), ("SM", SM), ("MD", MD), ("LG", LG), ("XL", XL), ("XXL", XXL), ("CARD", CARD)];
}

/// Control heights, icon sizes and layout widths.
pub mod size {
    use super::*;
    pub const TITLE_BAR_H: Pixels = px(48.);
    pub const SIDEBAR_W: Pixels = px(316.);
    /// Width reserved for the macOS traffic lights (at x=18, ~52px wide) plus a gap before
    /// the first title-bar button.
    pub const TRAFFIC_LIGHTS_W: Pixels = px(92.);
    pub const CONTENT_MAX_W: Pixels = px(760.);

    pub const ICON_XS: Pixels = px(11.);
    pub const ICON_SM: Pixels = px(12.);
    pub const ICON: Pixels = px(14.);
    pub const ICON_MD: Pixels = px(16.);

    pub const DOT: Pixels = px(7.);
    /// Small chips (attachments).
    pub const CHIP_H: Pixels = px(24.);
    /// Square icon buttons.
    pub const ICON_BUTTON: Pixels = px(28.);
    /// Transcript rows (thought, tool, working).
    pub const ROW_H: Pixels = px(30.);
    /// Sidebar thread rows.
    pub const THREAD_ROW_H: Pixels = px(32.);
    /// Round composer buttons (+, send).
    pub const ROUND_BUTTON: f32 = 32.;
    /// Activity spinners.
    pub const SPINNER_SM: f32 = 14.;
    pub const SPINNER_MD: f32 = 16.;
    pub const SPINNER_LG: f32 = 18.;
    pub const CONTEXT_RING: f32 = 16.;
    /// Fade at the bottom of the transcript, above the composer.
    pub const SCROLL_FADE_H: Pixels = px(72.);
    /// A scroll area follows its bottom while the user is within this distance of it.
    pub const FOLLOW_SLOP: f32 = 24.;
    /// Gap between the scroll-to-bottom button and the bottom of its scroll area (the top of
    /// the composer card).
    pub const SCROLL_BUTTON_INSET: Pixels = px(12.);
    /// Settings rows (label + description on the left, a control on the right).
    pub const SETTINGS_ROW_H: Pixels = px(60.);
    /// Settings dropdowns, so a section's controls line up.
    pub const SETTINGS_SELECT_W: Pixels = px(200.);
}

/// Type scale and font families.
pub mod text {
    use super::*;
    pub const UI_FONT: &str = ".SystemUIFont";
    pub const MONO_FONT: &str = "Menlo";

    pub const XS: Pixels = px(11.);
    pub const CODE: Pixels = px(12.);
    pub const SM: Pixels = px(12.5);
    pub const MD: Pixels = px(13.5);
    pub const BODY: Pixels = px(14.5);
    /// Page headings (Settings).
    pub const HEADING: Pixels = px(22.);
    pub const TITLE: Pixels = px(26.);
    /// Line height multipliers.
    pub const LEADING_TIGHT: f32 = 1.4;
    pub const LEADING: f32 = 1.5;
    pub const LEADING_PROSE: f32 = 1.6;

    pub const ALL: &[(&str, Pixels)] =
        &[("XS", XS), ("CODE", CODE), ("SM", SM), ("MD", MD), ("BODY", BODY), ("HEADING", HEADING), ("TITLE", TITLE)];
}

/// Map the tokens onto gpui-component's theme (call after `Theme::change`).
pub fn install(cx: &mut App) {
    use color::*;
    Theme::update(cx, |t| {
        t.font_family = text::UI_FONT.into();
        t.mono_font_family = text::MONO_FONT.into();
        t.font_size = px(14.);
        t.radius = radius::LG;
        t.radius_lg = radius::XXL;
        t.shadow = true;
        t.background = bg();
        t.foreground = text();
        t.muted = surface_2();
        t.muted_foreground = text_2();
        t.border = border();
        t.input = border_strong();
        t.ring = accent();
        t.caret = text();
        t.selection = selection();
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
        // No track: only the thumb shows, over whatever surface scrolls (the composer card too).
        t.scrollbar = gpui::transparent_black();
        t.scrollbar_thumb = border_strong();
        t.scrollbar_thumb_hover = scrollbar_thumb_hover();
        // Switches: accent track when on (`primary`), a quiet track when off, white thumb.
        t.switch = border_strong();
        t.switch_thumb = on_accent();
    });
}
