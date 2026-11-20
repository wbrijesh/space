//! The composer's `/` menu, and `/btw`, which opens the side chat.
//!
//! A text that is `/` and the start of a command's name lists the agent's commands (from its
//! `_space/status`) that begin with it. ↑/↓ move the selection, Tab completes `/name ` (to type
//! arguments), ⏎ or a click sends the selected command, Esc closes the menu until the text
//! changes. A command marked `side` (`/btw`) isn't sent as a prompt: typing it and a space opens
//! the side chat (see `side_chat`), focused, and what follows goes there.

use gpui::component::input::RopeExt as _;
use gpui::*;
use space_core::protocol::CommandInfo;
use space_ui::components::composer::{CommandMenu, RecallStep};

use crate::app::SpaceApp;

/// The `/` menu's state between frames.
#[derive(Debug, Default)]
pub(crate) struct SlashMenu {
    selected: usize,
    /// Esc closed the menu for this text; it opens again once the text changes.
    closed_for: Option<String>,
}

impl SlashMenu {
    /// The composer's text changed: the selection starts over.
    pub(crate) fn edited(&mut self, text: &str) {
        self.selected = 0;
        if self.closed_for.as_deref() != Some(text) {
            self.closed_for = None;
        }
    }
}

/// The commands `text` offers: it's `/` and the start of a name (no space yet).
pub(crate) fn matches<'a>(text: &str, commands: &'a [CommandInfo]) -> Vec<&'a CommandInfo> {
    let Some(name) = text.strip_prefix('/') else { return Vec::new() };
    if name.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    let name = name.to_lowercase();
    commands.iter().filter(|c| c.name.to_lowercase().starts_with(&name)).collect()
}

/// The text for the side chat when `text` (as sent) runs a side command: `/btw why?` → `why?`;
/// `/btw` → ``.
pub(crate) fn side_text<'a>(text: &'a str, commands: &[CommandInfo]) -> Option<&'a str> {
    let rest = text.trim().strip_prefix('/')?;
    let (name, question) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    commands.iter().any(|c| c.side && c.name == name).then(|| question.trim())
}

/// As it's typed: `text` is a side command and a space (or more after it), so the side chat
/// opens; what came after the space goes there. `/btw` alone isn't yet (it may become `/btwx`).
pub(crate) fn side_opening<'a>(text: &'a str, commands: &[CommandInfo]) -> Option<&'a str> {
    let rest = text.strip_prefix('/')?;
    let at = rest.find(char::is_whitespace)?;
    let (name, after) = rest.split_at(at);
    let space = after.chars().next()?.len_utf8();
    commands.iter().any(|c| c.side && c.name == name).then(|| &after[space..])
}

impl SpaceApp {
    /// The commands of the selected thread's agent: its session's, or (no session yet) the ones
    /// the agent listed last.
    pub(crate) fn commands(&self) -> Vec<CommandInfo> {
        let own = self.current().and_then(|t| t.state.status.commands.clone());
        own.or_else(|| self.agent_commands.clone()).unwrap_or_default()
    }

    /// The commands the menu lists now (empty: it's closed).
    fn menu_items(&self, cx: &App) -> Vec<CommandInfo> {
        let text = self.input.read(cx).value().to_string();
        if self.slash.closed_for.as_deref() == Some(text.as_str()) {
            return Vec::new();
        }
        matches(&text, &self.commands()).into_iter().cloned().collect()
    }

    /// The menu to draw, if it's open.
    pub(crate) fn command_menu(&self, cx: &App) -> Option<CommandMenu> {
        let items = self.menu_items(cx);
        (!items.is_empty()).then(|| CommandMenu { selected: self.slash.selected.min(items.len() - 1), items })
    }

    /// ↑/↓ while the menu is open: move the selection (wrapping). `false`: it's closed.
    pub(crate) fn step_command(&mut self, step: RecallStep, cx: &mut Context<Self>) -> bool {
        let n = self.menu_items(cx).len();
        if n == 0 {
            return false;
        }
        let at = self.slash.selected.min(n - 1);
        self.slash.selected = match step {
            RecallStep::Back => (at + n - 1) % n,
            RecallStep::Forward => (at + 1) % n,
        };
        cx.notify();
        true
    }

    /// Tab while the menu is open: the selected command, ready for its arguments.
    pub(crate) fn complete_command(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(menu) = self.command_menu(cx) else { return false };
        let name = menu.items[menu.selected].name.clone();
        self.set_composer_text(format!("/{name} "), window, cx);
        true
    }

    /// ⏎ while the menu is open: the composer gets the selected command, to be sent as it is.
    pub(crate) fn accept_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.command_menu(cx) {
            let name = menu.items[menu.selected].name.clone();
            self.set_composer_text(format!("/{name}"), window, cx);
        }
    }

    /// A command clicked in the menu: send it.
    pub(crate) fn run_command(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_composer_text(format!("/{name}"), window, cx);
        self.send(window, cx);
    }

    /// Esc while the menu is open: close it until the text changes. `false`: it wasn't open.
    pub(crate) fn close_command_menu(&mut self, cx: &mut Context<Self>) -> bool {
        if self.menu_items(cx).is_empty() {
            return false;
        }
        self.slash.closed_for = Some(self.input.read(cx).value().to_string());
        cx.notify();
        true
    }

    fn set_composer_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            let end = s.text().offset_to_position(s.text().len());
            s.set_cursor_position(end, window, cx);
        });
        self.slash.edited("");
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use space_core::protocol::CommandInfo;

    use super::{SlashMenu, matches, side_opening, side_text};

    fn commands() -> Vec<CommandInfo> {
        let c = |name: &str, side: bool| CommandInfo { name: name.into(), description: String::new(), hint: None, side };
        vec![c("btw", true), c("compact", false), c("context", false), c("goal", false)]
    }

    fn names(text: &str) -> Vec<String> {
        matches(text, &commands()).into_iter().map(|c| c.name.clone()).collect()
    }

    #[test]
    fn the_menu_lists_commands_starting_with_the_typed_name() {
        assert_eq!(names("/"), ["btw", "compact", "context", "goal"]);
        assert_eq!(names("/co"), ["compact", "context"]);
        assert_eq!(names("/CON"), ["context"]);
        assert!(names("/x").is_empty());
        // Arguments being typed, or no slash first: no menu.
        assert!(names("/compact keep the plan").is_empty());
        assert!(names("co").is_empty());
        assert!(names(" /co").is_empty());
    }

    #[test]
    fn side_commands_as_sent() {
        let cs = commands();
        assert_eq!(side_text("/btw why the retry?", &cs), Some("why the retry?"));
        assert_eq!(side_text("  /btw\n  what changed? ", &cs), Some("what changed?"));
        assert_eq!(side_text("/btw", &cs), Some(""));
        assert_eq!(side_text("/btwx why", &cs), None);
        assert_eq!(side_text("/compact", &cs), None);
        assert_eq!(side_text("btw why", &cs), None);
    }

    #[test]
    fn the_side_chat_opens_on_the_space_after_the_command() {
        let cs = commands();
        assert_eq!(side_opening("/btw", &cs), None, "not yet: it could still become another word");
        assert_eq!(side_opening("/btw ", &cs), Some(""));
        assert_eq!(side_opening("/btw why", &cs), Some("why"), "pasted with the question");
        assert_eq!(side_opening("/btw  two", &cs), Some(" two"));
        assert_eq!(side_opening("/btwx ", &cs), None);
        assert_eq!(side_opening("/compact ", &cs), None);
        assert_eq!(side_opening(" /btw ", &cs), None);
    }

    #[test]
    fn esc_closes_the_menu_until_the_text_changes() {
        let mut menu = SlashMenu { selected: 2, closed_for: Some("/co".into()) };
        menu.edited("/co");
        assert_eq!((menu.selected, menu.closed_for.as_deref()), (0, Some("/co")));
        menu.edited("/c");
        assert_eq!(menu.closed_for, None);
    }
}
