//! Space's UI layer: design tokens ([`theme`]), the app-wide [`clock::AnimationClock`],
//! presentational [`components`] (props in, callbacks out, no app state), and with the
//! `dev` feature their previews ([`preview`]) and the in-app [`gallery`].
//!
//! The pattern is documented in `docs/ui-development.md`.

pub mod actions;
pub mod assets;
pub mod clock;
pub mod components;
pub mod format;
pub mod theme;

#[cfg(feature = "dev")]
pub mod gallery;
#[cfg(feature = "dev")]
pub mod preview;

use std::rc::Rc;

use gpui::{App, ClickEvent, Window};

/// Click callback stored by components.
pub type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Dark theme with Space's tokens, plus the animation clock. Call once after `gpui::init`.
pub fn init(cx: &mut App) {
    gpui::component::Theme::change(gpui::component::ThemeMode::Dark, None, cx);
    theme::install(cx);
    cx.set_global(clock::AnimationClock::default());
}
