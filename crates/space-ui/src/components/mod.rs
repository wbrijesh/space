//! Presentational components. Each takes plain data and callbacks (never app state), so the
//! app and the gallery render the same code. With the `dev` feature each module also has a
//! `previews()` function listing named example states (see [`crate::preview`]).

pub mod composer;
pub mod file_tree;
pub mod file_viewer;
pub mod find;
pub mod follow_scroll;
pub mod images;
pub mod loaders;
pub mod menu;
pub mod message_actions;
pub mod model_picker;
pub mod open_in;
pub mod pastes;
pub mod quote;
pub mod right_pane;
pub mod settings;
pub mod shell;
pub mod terminal;
pub mod thread_row;
pub mod tip;
pub mod title_bar;
pub mod trajectory;
pub mod transcript;
pub mod turns;
pub mod widgets;
