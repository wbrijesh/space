//! Plugin secrets in the macOS login Keychain, through `/usr/bin/security`.
//!
//! Items are generic passwords: service `<prefix>.<plugin id>`, account = the setting key.
//! Going through the `security` tool (rather than the Security framework in-process) means
//! the items' access list names that Apple-signed tool, so reading them doesn't prompt again
//! every time a rebuilt (re-signed) Space launches. Values are written through `security -i`
//! on stdin, so they never appear in a process list.

use std::io::Write as _;
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};

/// Default service prefix of Space's Keychain items.
pub const DEFAULT_PREFIX: &str = "dev.brijesh.space.plugin";

/// Where plugin secrets live. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Secrets {
    prefix: String,
}

impl Default for Secrets {
    fn default() -> Self {
        Self::new(DEFAULT_PREFIX)
    }
}

impl Secrets {
    /// `prefix`: the Keychain service prefix (tests use their own, never the real one).
    pub fn new(prefix: impl Into<String>) -> Self {
        Self { prefix: prefix.into() }
    }

    fn service(&self, plugin_id: &str) -> String {
        format!("{}.{plugin_id}", self.prefix)
    }

    /// The stored value, or `None` when there is none.
    pub fn get(&self, plugin_id: &str, key: &str) -> Result<Option<String>> {
        let out = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", &self.service(plugin_id), "-a", key, "-w"])
            .stderr(Stdio::null())
            .output()
            .context("running /usr/bin/security")?;
        match out.status.code() {
            Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string())),
            // errSecItemNotFound
            Some(44) => Ok(None),
            code => bail!("reading the Keychain failed (security exited with {code:?})"),
        }
    }

    /// Store `value` (replacing any earlier one); `None` deletes it.
    pub fn set(&self, plugin_id: &str, key: &str, value: Option<&str>) -> Result<()> {
        let service = self.service(plugin_id);
        let Some(value) = value else {
            let status = Command::new("/usr/bin/security")
                .args(["delete-generic-password", "-s", &service, "-a", key])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("running /usr/bin/security")?;
            return match status.code() {
                Some(0) | Some(44) => Ok(()),
                code => bail!("deleting from the Keychain failed (security exited with {code:?})"),
            };
        };
        for part in [&service, key, value] {
            if part.is_empty() || part.chars().any(|c| c.is_control() || c == '"' || c == '\\') {
                bail!("the value can't be empty or contain quotes, backslashes or line breaks");
            }
        }
        let mut child = Command::new("/usr/bin/security")
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("running /usr/bin/security")?;
        let line = format!("add-generic-password -U -s \"{service}\" -a \"{key}\" -w \"{value}\"\n");
        child.stdin.take().context("security stdin")?.write_all(line.as_bytes())?;
        let out = child.wait_with_output()?;
        let err = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() || err.contains("error") {
            bail!("writing to the Keychain failed: {}", err.trim());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_a_test_service() {
        let secrets = Secrets::new("dev.brijesh.space.test");
        let plugin = format!("secrets-test-{}", std::process::id());
        assert_eq!(secrets.get(&plugin, "k").unwrap(), None);
        secrets.set(&plugin, "k", Some("sk-test_123")).unwrap();
        assert_eq!(secrets.get(&plugin, "k").unwrap().as_deref(), Some("sk-test_123"));
        secrets.set(&plugin, "k", Some("sk-test_456")).unwrap();
        assert_eq!(secrets.get(&plugin, "k").unwrap().as_deref(), Some("sk-test_456"));
        secrets.set(&plugin, "k", None).unwrap();
        assert_eq!(secrets.get(&plugin, "k").unwrap(), None);
        secrets.set(&plugin, "k", None).unwrap();
        assert!(secrets.set(&plugin, "k", Some("bad\"value")).is_err());
    }
}
