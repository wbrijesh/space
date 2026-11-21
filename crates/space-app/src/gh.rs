//! Pull requests through the GitHub CLI (`gh`): the current branch's pull request, opening one,
//! and editing its title, description and base. `gh` keeps its own sign-in; without it (or
//! signed out, or for a repository that isn't on GitHub) the Changes view offers no pull request
//! steps and says why. Calls block; callers run them in the background.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::repo::{command, run, tool_path};

const TIMEOUT: Duration = Duration::from_secs(30);
const CREATE_TIMEOUT: Duration = Duration::from_secs(90);

/// Whether pull requests can be made from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gh {
    Ready,
    /// `gh` isn't installed.
    Missing,
    /// `gh` isn't signed in.
    SignedOut,
    /// The repository's remotes aren't on a GitHub host `gh` knows.
    NotGitHub,
}

impl Gh {
    /// Why pull requests aren't offered; `None` when they are.
    pub fn why_not(&self) -> Option<&'static str> {
        match self {
            Gh::Ready => None,
            Gh::Missing => Some("Install the GitHub CLI (brew install gh) for pull requests"),
            Gh::SignedOut => Some("Sign in to the GitHub CLI (gh auth login) for pull requests"),
            Gh::NotGitHub => Some("This repository isn't on GitHub"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pr {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub url: String,
    pub state: PrState,
    pub base_ref_name: String,
    #[serde(default)]
    pub is_draft: bool,
}

fn gh_path() -> Option<PathBuf> {
    std::env::split_paths(&tool_path()).map(|d| d.join("gh")).find(|p| p.is_file())
}

fn gh(root: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let program = gh_path().ok_or_else(|| "The GitHub CLI (gh) isn't installed".to_string())?;
    let mut c = command(program.to_str().unwrap_or("gh"), root);
    c.env("GH_PROMPT_DISABLED", "1").env("NO_COLOR", "1").args(args);
    run(c, timeout)
}

/// Whether `gh` can make pull requests from `root`, and the current branch's pull request (the
/// latest, open or not) when it can.
pub fn status(root: &Path) -> (Gh, Option<Pr>) {
    if gh_path().is_none() {
        return (Gh::Missing, None);
    }
    match gh(root, &["pr", "view", "--json", "number,title,body,url,state,baseRefName,isDraft"], TIMEOUT) {
        Ok(json) => (Gh::Ready, serde_json::from_str(&json).ok()),
        Err(e) => (classify(&e), None),
    }
}

/// What a failed `gh pr view` means.
fn classify(error: &str) -> Gh {
    let e = error.to_lowercase();
    if e.contains("no pull requests found") || e.contains("no open pull requests") {
        Gh::Ready
    } else if e.contains("gh auth login") || e.contains("not logged") || e.contains("authentication") {
        Gh::SignedOut
    } else if e.contains("none of the git remotes") || e.contains("not a github") || e.contains("no git remotes") {
        Gh::NotGitHub
    } else {
        // Offline, or GitHub having a moment: offer pull requests, and let a step say what fails.
        Gh::Ready
    }
}

/// Open a pull request from `branch` (pushed) into `base`; its URL.
pub fn create(root: &Path, branch: &str, base: &str, title: &str, body: &str) -> Result<String, String> {
    let out = gh(root, &["pr", "create", "--head", branch, "--base", base, "--title", title, "--body", body], CREATE_TIMEOUT)?;
    Ok(out.lines().rev().find(|l| l.starts_with("http")).unwrap_or(out.trim()).trim().to_string())
}

/// Change pull request `number`'s title, description and base.
pub fn edit(root: &Path, number: u64, title: &str, body: &str, base: &str) -> Result<(), String> {
    gh(root, &["pr", "edit", &number.to_string(), "--title", title, "--body", body, "--base", base], CREATE_TIMEOUT).map(drop)
}

/// Claude's reply as a pull request: the first line is the title, the rest the description.
pub fn parse_pr(reply: &str) -> (String, String) {
    let (_, text) = crate::repo::parse_message(reply, false);
    let (title, body) = text.split_once('\n').unwrap_or((&text, ""));
    let title = title.trim().trim_start_matches('#').trim().trim_start_matches("Title:").trim();
    (title.to_string(), body.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_gh_says() {
        assert_eq!(classify("no pull requests found for branch \"main\""), Gh::Ready);
        assert_eq!(classify("To get started with GitHub CLI, please run:  gh auth login"), Gh::SignedOut);
        assert_eq!(classify("none of the git remotes configured for this repository point to a known GitHub host"), Gh::NotGitHub);
        let pr: Pr = serde_json::from_str(r#"{"number":7,"title":"T","body":"B","url":"https://x/7","state":"MERGED","baseRefName":"main","isDraft":false}"#).unwrap();
        assert_eq!((pr.number, pr.state, pr.base_ref_name.as_str()), (7, PrState::Merged, "main"));
    }

    #[test]
    fn reads_claudes_pull_request() {
        assert_eq!(parse_pr("# Resizable tree\n\nDrag its edge.\n\n- one"), ("Resizable tree".into(), "Drag its edge.\n\n- one".into()));
        assert_eq!(parse_pr("Title: Fix\n"), ("Fix".into(), String::new()));
    }
}
