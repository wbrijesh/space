//! `space-titles`: names a thread after its first message. It asks the thread's agent for one
//! short completion (`agent/complete`), with the thread's own model unless the "Title model"
//! setting picks another, and tidies the reply into a few words. Space shows the first line of
//! the message until the title arrives, and keeps it if none does.

use std::sync::{Mutex, OnceLock};

use space_plugin::protocol::service::*;
use space_plugin::{Host, Plugin, RpcError};

const MODEL: &str = "model";
/// How much of the first message the model sees.
const MESSAGE_CHARS: usize = 500;
/// A reply longer than this isn't a title.
const MAX_WORDS: usize = 8;

/// The prompt for a title of the thread that starts with `message`.
fn prompt(message: &str) -> String {
    let message: String = message.trim().chars().take(MESSAGE_CHARS).collect();
    format!(
        "Write a title of 3 to 6 words for a conversation that starts with the message below. \
         Reply with the title only, without quotes or a full stop.\n\n<message>\n{message}\n</message>"
    )
}

/// The title in a model's reply: its first line without quotes, a "Title:" label or trailing
/// punctuation. `None` when nothing title-like is left.
fn tidy(reply: &str) -> Option<String> {
    let line = reply.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.strip_prefix("Title:").or_else(|| line.strip_prefix("title:")).unwrap_or(line);
    let quotes: &[char] = &['"', '\'', '`', '*', '“', '”', '‘', '’', '#'];
    let line = line.trim().trim_matches(quotes).trim().trim_end_matches(['.', '!', ':', ';', ',']).trim_matches(quotes).trim();
    let words: Vec<&str> = line.split_whitespace().collect();
    (!words.is_empty() && words.len() <= MAX_WORDS).then(|| words.join(" "))
}

/// The "Title model" setting as (provider, model id); unset: the thread's own model.
fn chosen_model(values: &SettingValues) -> Option<(String, String)> {
    let (provider, id) = values.get(MODEL)?.split_once('/')?;
    Some((provider.to_string(), id.to_string()))
}

struct Titles {
    model: Mutex<Option<(String, String)>>,
    host: OnceLock<Host>,
}

impl Plugin for Titles {
    async fn initialize(&self, init: Initialize, host: Host) -> Result<Initialized, RpcError> {
        *self.model.lock().unwrap() = chosen_model(&init.settings);
        let _ = self.host.set(host);
        Ok(Initialized::default())
    }

    fn settings_schema(&self) -> SettingsSchema {
        SettingsSchema {
            fields: vec![SettingField {
                key: MODEL.into(),
                label: "Title model".into(),
                description: "Names new threads after their first message.".into(),
                kind: SettingKind::Model,
                placeholder: None,
                default: None,
            }],
        }
    }

    fn settings_changed(&self, changed: SettingsChanged) {
        *self.model.lock().unwrap() = chosen_model(&changed.values);
    }

    async fn title(&self, request: TitleRequest) -> Result<Title, RpcError> {
        let host = self.host.get().ok_or_else(|| RpcError::unavailable("not initialized"))?;
        let (provider, model) = self.model.lock().unwrap().clone().unzip();
        let complete = Complete { session_id: request.session_id, prompt: prompt(&request.first_message), provider, model };
        let reply = host.request(AGENT_COMPLETE, serde_json::to_value(complete).expect("serializable")).await?;
        let reply: CompleteResult = serde_json::from_value(reply).map_err(RpcError::invalid_params)?;
        let title = tidy(&reply.text).ok_or_else(|| RpcError::internal(format!("not a title: {:?}", reply.text)))?;
        Ok(Title { title })
    }
}

fn main() {
    space_plugin::run(Titles { model: Mutex::new(None), host: OnceLock::new() });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_become_titles() {
        assert_eq!(tidy("\"Fix the login redirect.\"\n").as_deref(), Some("Fix the login redirect"));
        assert_eq!(tidy("Title: **Rust plugin host**").as_deref(), Some("Rust plugin host"));
        assert_eq!(tidy("\n  “Thread persistence”  \nmore").as_deref(), Some("Thread persistence"));
        assert_eq!(tidy("   "), None);
        assert_eq!(tidy("This reply goes on far too long to be any kind of title at all"), None);
    }

    #[test]
    fn the_prompt_has_the_start_of_the_message() {
        let long = "word ".repeat(300);
        let p = prompt(&long);
        assert!(p.contains("3 to 6 words"));
        assert!(p.len() < MESSAGE_CHARS + 300);
    }

    #[test]
    fn the_model_setting() {
        assert_eq!(chosen_model(&SettingValues::new()), None);
        let values: SettingValues = [(MODEL.to_string(), "claude-cli/claude-haiku-5-5".to_string())].into();
        assert_eq!(chosen_model(&values), Some(("claude-cli".into(), "claude-haiku-5-5".into())));
        // Model ids may contain slashes; providers don't.
        let values: SettingValues = [(MODEL.to_string(), "openrouter/anthropic/claude".to_string())].into();
        assert_eq!(chosen_model(&values), Some(("openrouter".into(), "anthropic/claude".into())));
    }
}
