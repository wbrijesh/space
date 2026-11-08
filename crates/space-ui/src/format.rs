//! Pure presentation helpers: friendly names, path/branch labels, thread titles and previews.

use std::path::{Path, PathBuf};

use space_core::protocol::ModelInfo;
use space_core::{SessionState, TranscriptItem};

/// "Opus 5.5" for `claude-opus-5-5` / "Claude Opus 5.5": drop the vendor prefix and any
/// "(latest)" suffix from the provider's display name, else derive it from the id.
pub fn model_label(id: &str, info: Option<&ModelInfo>) -> String {
    if let Some(name) = info.and_then(|m| m.name.as_deref()) {
        let name = name.trim();
        let name = name.strip_suffix("(latest)").unwrap_or(name).trim_end();
        let name = name.strip_prefix("Claude ").unwrap_or(name);
        if !name.is_empty() {
            return name.to_string();
        }
    }
    // claude-opus-5-5 -> Opus 5.5; other ids: capitalized words, version digits joined by '.'.
    let id = id.strip_prefix("claude-").unwrap_or(id);
    let mut words: Vec<String> = Vec::new();
    let mut version: Vec<&str> = Vec::new();
    for part in id.split(['-', '_']) {
        if !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()) && part.len() < 4 {
            version.push(part);
        } else if !part.is_empty() {
            let mut chars = part.chars();
            let first = chars.next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            words.push(first + chars.as_str());
        }
    }
    if !version.is_empty() {
        words.push(version.join("."));
    }
    words.join(" ")
}

/// "medium" -> "Medium", "xhigh" -> "Extra high".
pub fn effort_label(level: &str) -> String {
    match level {
        "xhigh" => "Extra high".into(),
        "off" => "No thinking".into(),
        _ => {
            let mut chars = level.chars();
            chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
        }
    }
}

/// `/Users/me/projects/space` -> `~/projects/space`.
pub fn abbreviate_home(path: &Path) -> String {
    if let Some(home) = dirs_home()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
    }
    path.display().to_string()
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Last path component (the project name).
pub fn dir_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Current git branch of the repository containing `dir` (short sha when detached).
pub fn git_branch(dir: &Path) -> Option<String> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let dot_git = d.join(".git");
        let git_dir = if dot_git.is_dir() {
            Some(dot_git)
        } else if dot_git.is_file() {
            // Worktrees / submodules: `gitdir: <path>`.
            let text = std::fs::read_to_string(&dot_git).ok()?;
            let p = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
            Some(if p.is_absolute() { p } else { d.join(p) })
        } else {
            None
        };
        if let Some(git_dir) = git_dir {
            let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
            let head = head.trim();
            return Some(match head.strip_prefix("ref: refs/heads/") {
                Some(branch) => branch.to_string(),
                None => head.chars().take(7).collect(),
            });
        }
        cur = d.parent();
    }
    None
}

/// Collapse whitespace into single spaces.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Thread title: the first user message, first non-empty line.
pub fn thread_title(state: &SessionState) -> Option<String> {
    state.items.iter().find_map(|i| match i {
        TranscriptItem::User { text, .. } => {
            text.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| truncate_chars(l, 120))
        }
        _ => None,
    })
}

/// What the sidebar shows under a thread title.
#[derive(Debug, Clone, PartialEq)]
pub enum Preview {
    /// Latest assistant text.
    Text(String),
    Working(Option<String>),
    NeedsInput,
    Error(String),
    Empty,
}

pub fn thread_preview(state: &SessionState) -> Preview {
    if state.pending_permission().is_some() {
        return Preview::NeedsInput;
    }
    if state.running {
        let tool = state.items.iter().rev().find_map(|i| match i {
            TranscriptItem::Tool(t) if t.status == space_core::ToolStatus::InProgress => Some(tool_parts(&t.title)),
            _ => None,
        });
        return Preview::Working(tool.map(|(name, detail)| one_line(&format!("{name} {detail}"))));
    }
    // An error after the last user message wins.
    for item in state.items.iter().rev() {
        match item {
            TranscriptItem::Error(e) => return Preview::Error(truncate_chars(&one_line(&clean_error(e)), 200)),
            TranscriptItem::Agent(t) => {
                let plain = strip_markdown(t);
                if !plain.is_empty() {
                    return Preview::Text(truncate_chars(&plain, 220));
                }
            }
            TranscriptItem::User { .. } => break,
            _ => {}
        }
    }
    Preview::Empty
}

/// The readable part of a host error: without the `session/prompt failed:` wrapper and
/// the plugin stderr tail.
pub fn clean_error(message: &str) -> String {
    let main = message.split("\nplugin stderr:").next().unwrap_or(message).trim();
    let main = main.strip_prefix("session/prompt failed: ").unwrap_or(main);
    let main = main.strip_prefix("Internal error: ").unwrap_or(main);
    main.trim().trim_matches('"').to_string()
}

/// Rough markdown → plain text for one-line previews.
pub fn strip_markdown(text: &str) -> String {
    let mut out = Vec::new();
    let mut in_code = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code || l.is_empty() {
            continue;
        }
        let l = l.trim_start_matches(['#', '>', ' ']);
        let l = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")).unwrap_or(l);
        out.push(l.replace(['*', '`'], ""));
    }
    one_line(&out.join(" "))
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// pi-style tool titles `bash: ls -la` → ("Bash", "ls -la"); other titles → (title, "").
pub fn tool_parts(title: &str) -> (String, String) {
    match title.split_once(": ") {
        Some((name, detail)) if !name.is_empty() && !name.contains(' ') => {
            let mut chars = name.chars();
            let name = chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default();
            (name, detail.to_string())
        }
        _ => (title.to_string(), String::new()),
    }
}

/// Context-ring tooltip: "Context: 18.2k / 1M tokens · 2%".
pub fn context_usage(tokens: u64, window: Option<u64>) -> String {
    match window {
        Some(window) if window > 0 => {
            let pct = tokens as f64 / window as f64 * 100.;
            let pct = if tokens > 0 && pct < 1. { "<1%".to_string() } else { format!("{pct:.0}%") };
            format!("Context: {} / {} tokens · {pct}", compact_tokens(tokens), compact_tokens(window))
        }
        _ => "Context usage not reported yet".to_string(),
    }
}

/// 18234 → "18.2k", 1000000 → "1M".
pub fn compact_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        let v = n as f64 / 1_000_000.;
        if v.fract() < 0.05 { format!("{v:.0}M") } else { format!("{v:.1}M") }
    } else if n >= 1000 {
        let v = n as f64 / 1000.;
        if v >= 100. || v.fract() < 0.05 { format!("{v:.0}k") } else { format!("{v:.1}k") }
    } else {
        n.to_string()
    }
}

/// "45s", "2m 14s", "1h 5m": a short, human duration (whole seconds).
pub fn short_duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

/// "Last tried 2m 14s ago · next try in 2m 46s", from Unix milliseconds; `None` when neither
/// time is known. A next try that's due reads "trying again now".
pub fn attempt_times(last_ms: Option<u64>, next_ms: Option<u64>, now_ms: u64) -> Option<String> {
    let ms = std::time::Duration::from_millis;
    let last = last_ms.map(|t| format!("Last tried {} ago", short_duration(ms(now_ms.saturating_sub(t)))));
    let next = next_ms.map(|t| match t.checked_sub(now_ms).filter(|d| *d >= 1000) {
        Some(wait) => format!("next try in {}", short_duration(ms(wait))),
        None => "trying again now".to_string(),
    });
    match (last, next) {
        (Some(l), Some(n)) => Some(format!("{l} · {n}")),
        (Some(one), None) | (None, Some(one)) => Some(one),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use space_core::SessionEvent;

    fn info(name: &str) -> ModelInfo {
        ModelInfo { provider: "claude-cli".into(), id: "x".into(), name: Some(name.into()) }
    }

    #[test]
    fn model_labels() {
        assert_eq!(model_label("claude-opus-5-5", Some(&info("Claude Opus 5.5"))), "Opus 5.5");
        assert_eq!(model_label("claude-haiku-4-5", Some(&info("Claude Haiku 4.5 (latest)"))), "Haiku 4.5");
        assert_eq!(model_label("claude-opus-5-5", None), "Opus 5.5");
        assert_eq!(model_label("claude-sonnet-5-5", None), "Sonnet 5.5");
        assert_eq!(model_label("claude-haiku-5-5", None), "Haiku 5.5");
        assert_eq!(model_label("claude-fable-5", None), "Fable 5");
        assert_eq!(model_label("stub-model", Some(&info("Stub Model"))), "Stub Model");
        assert_eq!(model_label("gpt-x-mini", None), "Gpt X Mini");
    }

    #[test]
    fn effort_labels() {
        assert_eq!(effort_label("medium"), "Medium");
        assert_eq!(effort_label("xhigh"), "Extra high");
        assert_eq!(effort_label("max"), "Max");
    }

    #[test]
    fn titles_and_previews() {
        let mut s = SessionState::new("s");
        assert_eq!(thread_preview(&s), Preview::Empty);
        s.apply(&SessionEvent::UserMessage { text: "\n  Fix the tests\nplease".into(), attachments: vec![] });
        s.apply(&SessionEvent::TurnStarted);
        assert_eq!(thread_title(&s).as_deref(), Some("Fix the tests"));
        assert_eq!(thread_preview(&s), Preview::Working(None));
        s.apply(&SessionEvent::AgentText { text: "## Done\n\nAll **green**:\n```\nok\n```\n- `cargo test`".into() });
        s.apply(&SessionEvent::TurnEnded { stop_reason: "end_turn".into() });
        assert_eq!(thread_preview(&s), Preview::Text("Done All green: cargo test".into()));
        s.apply(&SessionEvent::Error { message: "boom".into() });
        assert_eq!(thread_preview(&s), Preview::Error("boom".into()));
    }

    #[test]
    fn misc() {
        assert_eq!(tool_parts("bash: ls -la"), ("Bash".into(), "ls -la".into()));
        assert_eq!(tool_parts("Run tests"), ("Run tests".into(), String::new()));
        assert_eq!(
            clean_error("session/prompt failed: The model request failed\nplugin stderr:\nx"),
            "The model request failed"
        );
        assert_eq!(compact_tokens(18_234), "18.2k");
        assert_eq!(compact_tokens(1_000_000), "1M");
        assert_eq!(compact_tokens(200_000), "200k");
        assert_eq!(truncate_chars("abcdef", 4), "abc…");
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(git_branch(repo).is_some_and(|b| !b.is_empty()));
    }

    #[test]
    fn durations_and_attempt_times() {
        use std::time::Duration;
        assert_eq!(short_duration(Duration::from_secs(45)), "45s");
        assert_eq!(short_duration(Duration::from_secs(134)), "2m 14s");
        assert_eq!(short_duration(Duration::from_secs(3900)), "1h 5m");
        let now = 1_000_000;
        assert_eq!(
            attempt_times(Some(now - 134_000), Some(now + 166_000), now).as_deref(),
            Some("Last tried 2m 14s ago · next try in 2m 46s")
        );
        assert_eq!(attempt_times(Some(now - 1_000), Some(now - 5), now).as_deref(), Some("Last tried 1s ago · trying again now"));
        assert_eq!(attempt_times(None, None, now), None);
    }
}
