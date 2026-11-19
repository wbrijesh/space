//! The composer's `/` menu, and side questions (`/btw`).
//!
//! A text that is `/` and the start of a command's name lists the agent's commands (from its
//! `_space/status`) that begin with it. ↑/↓ move the selection, Tab completes `/name ` (to type
//! arguments), ⏎ or a click sends the selected command, Esc closes the menu until the text
//! changes. A command marked `side` (`/btw`) isn't sent as a prompt: the rest of its text is a
//! side question, answered beside the conversation (also while a turn runs) above the composer;
//! the command alone shows the last answer again.

use gpui::component::input::RopeExt as _;
use gpui::*;
use space_core::protocol::CommandInfo;
use space_ui::components::composer::{CommandMenu, RecallStep, SideView};

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

/// The side question in `text`, when it runs a side command: `/btw why?` → `why?`; `/btw` → ``.
pub(crate) fn side_question<'a>(text: &'a str, commands: &[CommandInfo]) -> Option<&'a str> {
    let rest = text.trim().strip_prefix('/')?;
    let (name, question) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    commands.iter().any(|c| c.side && c.name == name).then(|| question.trim())
}

impl SpaceApp {
    /// The commands of the selected thread's agent: its session's, or (no session yet) the ones
    /// the agent listed last.
    fn commands(&self) -> Vec<CommandInfo> {
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

    /// Send `text` as a side question when it runs a side command. `false`: it doesn't.
    pub(crate) fn send_side_question(&mut self, key: u64, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let commands = self.commands();
        let Some(question) = side_question(text, &commands).map(str::to_string) else { return false };
        self.input.update(cx, |s, cx| s.clean(window, cx));
        let Some(thread) = self.thread_mut(key) else { return true };
        thread.draft.clear();
        if question.is_empty() {
            // The command alone: the last answer again.
            thread.state.dismiss_side(false);
        } else if let Some(session) = thread.live_session().cloned() {
            cx.background_spawn(async move {
                if let Err(e) = session.ask_aside(question).await {
                    tracing::info!("side question: {e:#}");
                }
            })
            .detach();
        } else {
            self.banner = Some("Side questions are about a conversation: send a message first.".into());
        }
        cx.notify();
        true
    }

    /// The side question to show above the composer.
    pub(crate) fn side_view(&self) -> Option<SideView> {
        let side = self.current()?.state.side()?;
        Some(SideView {
            question: side.question.clone().into(),
            answer: side.answer.clone().map(Into::into),
            error: side.error.clone().map(Into::into),
        })
    }

    /// Close the side question (its × or Esc). `false`: none was shown.
    pub(crate) fn close_side(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(key) = self.selected else { return false };
        let Some(thread) = self.thread_mut(key).filter(|t| t.state.side().is_some()) else { return false };
        thread.state.dismiss_side(true);
        cx.notify();
        true
    }
}

#[cfg(test)]
mod tests {
    use space_core::protocol::CommandInfo;

    use super::{SlashMenu, matches, side_question};

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
    fn side_questions() {
        let cs = commands();
        assert_eq!(side_question("/btw why the retry?", &cs), Some("why the retry?"));
        assert_eq!(side_question("  /btw\n  what changed? ", &cs), Some("what changed?"));
        assert_eq!(side_question("/btw", &cs), Some(""));
        assert_eq!(side_question("/btwx why", &cs), None);
        assert_eq!(side_question("/compact", &cs), None);
        assert_eq!(side_question("btw why", &cs), None);
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
