//! Presentational components. Each takes plain data and callbacks (never app state), so the
//! app and the gallery render the same code. With the `dev` feature each module also has a
//! `previews()` function listing named example states (see [`crate::preview`]).

pub mod changes;
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
pub mod notice;
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// No hover or group-hover style changes `display` (`hidden`, `flex`, ...). GPUI decides
    /// hover separately for prepaint (as of the last mouse move) and for paint (this frame's hit
    /// test); when rows move under a still pointer the two differ, and an element hidden at
    /// prepaint but shown at paint has its children painted without a prepaint, which panics
    /// ("must call prepaint before paint"). Show or hide by hover with `invisible`/`visible`
    /// (laid out either way), or choose in render from `on_hover` state (see `thread_row`).
    #[test]
    fn hover_styles_never_change_display() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut offenders = Vec::new();
        for dir in [root.join("src"), root.join("../space-app/src")] {
            scan(&dir, &mut offenders);
        }
        assert!(offenders.is_empty(), "hover styles that change display:\n{}", offenders.join("\n"));
    }

    fn scan(dir: &Path, offenders: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan(&path, offenders);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let source = std::fs::read_to_string(&path).unwrap();
                for (at, _) in source.match_indices("hover(") {
                    let style = call_args(&source[at + "hover(".len()..]);
                    let display = ["hidden()", "flex()", "block()", "grid()", "display"];
                    if source[..at].ends_with(['.', '_']) && display.iter().any(|d| style.contains(d)) {
                        let line = source[..at].lines().count();
                        offenders.push(format!("{}:{line}: {}", path.display(), style.trim()));
                    }
                }
            }
        }
    }

    /// The text up to the parenthesis that closes a call whose `(` was just before `rest`.
    fn call_args(rest: &str) -> &str {
        let mut depth = 1;
        for (i, c) in rest.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return &rest[..i];
                    }
                }
                _ => {}
            }
        }
        rest
    }
}
