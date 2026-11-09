//! The terminal seam: a plugin that provides [`CAP_TERMINAL`](crate::manifest::CAP_TERMINAL)
//! runs shells on PTYs for the app's terminal pane. The plugin owns the processes; the app owns
//! the screen (it parses the bytes and draws them).
//!
//! | Method | Direction | |
//! |---|---|---|
//! | `terminal/open` | host → plugin | start a shell: [`TerminalOpen`] → [`TerminalOpened`] |
//! | `terminal/input` (notification) | host → plugin | keys and pastes: [`TerminalInput`] |
//! | `terminal/resize` (notification) | host → plugin | [`TerminalResize`] |
//! | `terminal/attach` | host → plugin | everything kept so far: [`TerminalRef`] → [`TerminalReplay`] |
//! | `terminal/list` | host → plugin | the terminals it runs: → [`TerminalList`] |
//! | `terminal/close` | host → plugin | stop the shell and what it started: [`TerminalRef`] |
//! | `terminal/output` (notification) | plugin → host | [`TerminalOutput`], numbered in order |
//! | `terminal/title` (notification) | plugin → host | the program in front changed: [`TerminalTitle`] |
//! | `terminal/exited` (notification) | plugin → host | the shell ended: [`TerminalExited`] |
//!
//! Input and output are notifications so they keep their order. Output is numbered (`seq`) so a
//! replay (`terminal/attach`) and the live stream join without gaps or repeats: after a replay
//! up to `seq`, output numbered `seq` or lower is already on screen.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const TERMINAL_OPEN: &str = "terminal/open";
pub const TERMINAL_INPUT: &str = "terminal/input";
pub const TERMINAL_RESIZE: &str = "terminal/resize";
pub const TERMINAL_ATTACH: &str = "terminal/attach";
pub const TERMINAL_LIST: &str = "terminal/list";
pub const TERMINAL_CLOSE: &str = "terminal/close";
pub const TERMINAL_OUTPUT: &str = "terminal/output";
pub const TERMINAL_TITLE: &str = "terminal/title";
pub const TERMINAL_EXITED: &str = "terminal/exited";

/// Raw bytes, as base64 in JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        STANDARD.decode(text.as_bytes()).map(Bytes).map_err(serde::de::Error::custom)
    }
}

/// Start a shell for a thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpen {
    pub thread_id: String,
    /// Where the shell starts (the thread's project folder).
    pub cwd: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpened {
    pub terminal_id: String,
    /// What's running, for the tab (the shell's name to start with).
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInput {
    pub terminal_id: String,
    pub data: Bytes,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResize {
    pub terminal_id: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRef {
    pub terminal_id: String,
}

/// The output kept so far (the newest part, at most a few MB), and where the live stream picks
/// up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalReplay {
    pub data: Bytes,
    /// The last output included.
    pub seq: u64,
    pub title: String,
    /// `None` while the shell runs; its exit code once it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// It ended (the exit code may be unknown).
    #[serde(default)]
    pub exited: bool,
}

/// One terminal the plugin runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    pub terminal_id: String,
    pub thread_id: String,
    pub title: String,
    #[serde(default)]
    pub exited: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalList {
    pub terminals: Vec<TerminalInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutput {
    pub terminal_id: String,
    /// Numbered from 1, per terminal.
    pub seq: u64,
    pub data: Bytes,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalTitle {
    pub terminal_id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalExited {
    pub terminal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_travel_as_base64() {
        let out = TerminalOutput { terminal_id: "t1".into(), seq: 3, data: Bytes(b"\x1b[31mhi\r\n".to_vec()) };
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["data"], "G1szMW1oaQ0K");
        assert_eq!(json["terminalId"], "t1");
        assert_eq!(serde_json::from_value::<TerminalOutput>(json).unwrap(), out);
        assert!(serde_json::from_value::<Bytes>(serde_json::json!("not base64!")).is_err());
    }
}
