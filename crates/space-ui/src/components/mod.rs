//! Presentational components. Each takes plain data and callbacks (never app state), so the
//! app and the gallery render the same code. With the `dev` feature each module also has a
//! `previews()` function listing named example states (see [`crate::preview`]).

pub mod composer;
pub mod follow_scroll;
pub mod images;
pub mod loaders;
pub mod message_actions;
pub mod settings;
pub mod shell;
pub mod thread_row;
pub mod title_bar;
pub mod trajectory;
pub mod transcript;
pub mod widgets;
