//! Renaming a thread (⌘R, File ▸ Rename Thread…, or a sidebar row's menu): a native sheet with
//! the title in a text field ([`crate::text_prompt`]). The name can't be empty. A renamed thread
//! keeps your name: a title generated afterwards doesn't replace it, and it's saved as yours.

use gpui::*;

use crate::app::SpaceApp;
use crate::text_prompt::{self, TextPrompt};

/// The longest a name can be (in characters).
const MAX_NAME: usize = 200;

/// A typed name as a title: on one line, without surrounding spaces, at most [`MAX_NAME`]
/// characters. `None` when nothing is left.
pub(crate) fn clean_name(typed: &str) -> Option<String> {
    let one_line = typed.split_whitespace().collect::<Vec<_>>().join(" ");
    let name: String = one_line.chars().take(MAX_NAME).collect();
    let name = name.trim().to_string();
    (!name.is_empty()).then_some(name)
}

impl SpaceApp {
    /// ⌘R: rename the selected thread.
    pub(crate) fn rename_current_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings {
            return;
        }
        if let Some(key) = self.selected {
            self.rename_thread(key, window, cx);
        }
    }

    /// Ask for a new name for the thread. A new, empty thread has nothing to rename (it isn't
    /// listed yet).
    pub(crate) fn rename_thread(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key).filter(|t| !t.is_empty()) else { return };
        let title = thread.title();
        let answer = text_prompt::ask(
            window,
            TextPrompt { title: "Rename Thread", detail: "Enter a new name for this thread.", initial: &title, accept: "Rename" },
        );
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Some(name)) = answer.await {
                let _ = this.update(cx, |this, cx| this.set_thread_name(key, &name, cx));
            }
        })
        .detach();
    }

    /// Give the thread your name for it, save it, and tell plugins.
    pub(crate) fn set_thread_name(&mut self, key: u64, typed: &str, cx: &mut Context<Self>) {
        let Some(name) = clean_name(typed) else { return };
        let Some(thread) = self.thread_mut(key) else { return };
        if thread.renamed && thread.name.as_deref() == Some(name.as_str()) {
            return;
        }
        thread.name = Some(name.clone());
        thread.renamed = true;
        let session_id = thread.session.as_ref().map(|s| s.id().to_string());
        if let Some(id) = session_id {
            self.host.thread_renamed(&id, &name);
        }
        if self.saving_threads() {
            self.save_thread(key, cx);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::clean_name;

    #[test]
    fn names_are_one_trimmed_line() {
        assert_eq!(clean_name("  Fix the login redirect  ").as_deref(), Some("Fix the login redirect"));
        assert_eq!(clean_name("two\nlines\tand  gaps").as_deref(), Some("two lines and gaps"));
        assert_eq!(clean_name(""), None);
        assert_eq!(clean_name(" \n\t "), None);
        assert_eq!(clean_name(&"é".repeat(300)).map(|n| n.chars().count()), Some(200));
    }
}
