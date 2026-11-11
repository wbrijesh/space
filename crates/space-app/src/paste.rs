//! Long pastes. Plain text of more than [`LONG_PASTE`] characters pasted into the composer
//! doesn't go into the input: it becomes a "Pasted text" attachment. It's kept in the thread's
//! attachments folder as `<sha256>.txt` when sent, and the agent gets the text inline.

use std::path::Path;

use gpui::SharedString;
use sha2::{Digest as _, Sha256};
use space_core::TextRef;

/// Longer pastes (in characters) become attachments.
pub const LONG_PASTE: usize = 4096;

/// A long paste in a draft or a queued message.
#[derive(Clone, Debug)]
pub struct Paste {
    pub text: SharedString,
    pub chars: u64,
    /// Lowercase hex sha256 of the text's UTF-8 bytes.
    pub sha256: String,
}

impl Paste {
    pub fn new(text: impl Into<SharedString>) -> Self {
        let text = text.into();
        let sha256 = Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        Self { chars: text.chars().count() as u64, text, sha256 }
    }

    /// `<sha256>.txt`.
    pub fn file_name(&self) -> String {
        format!("{}.txt", self.sha256)
    }
}

/// Whether pasted text is long enough to become an attachment.
pub fn is_long(text: &str) -> bool {
    text.len() > LONG_PASTE && text.chars().nth(LONG_PASTE).is_some()
}

/// Write the paste into `dir` as [`Paste::file_name`] unless it's already there.
pub fn save(p: &Paste, dir: &Path) -> std::io::Result<TextRef> {
    let path = crate::images::write_once(dir, &p.file_name(), p.text.as_bytes())?;
    Ok(TextRef { path, mime: "text/plain".into(), chars: p.chars })
}

/// Save a message's pastes and images (in the background, before it's sent).
pub fn save_all(images: &[crate::images::Prepared], pastes: &[Paste], dir: &Path) -> std::io::Result<(Vec<space_core::ImageRef>, Vec<TextRef>)> {
    let images = images.iter().map(|i| crate::images::save(i, dir)).collect::<std::io::Result<_>>()?;
    let texts = pastes.iter().map(|p| save(p, dir)).collect::<std::io::Result<_>>()?;
    Ok((images, texts))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn only_pastes_over_4096_characters_are_long() {
        assert!(!is_long(&"a".repeat(LONG_PASTE)));
        assert!(is_long(&"a".repeat(LONG_PASTE + 1)));
        // Characters, not bytes: 4096 two-byte characters are 8192 bytes, and not long.
        assert!(!is_long(&"é".repeat(LONG_PASTE)));
        assert!(is_long(&"é".repeat(LONG_PASTE + 1)));
        assert!(!is_long(""));
    }

    #[test]
    fn saved_once_by_its_hash() {
        let p = Paste::new("é".repeat(5000));
        assert_eq!((p.chars, p.sha256.len()), (5000, 64));
        let dir = tempfile::tempdir().unwrap();
        let r = save(&p, dir.path()).unwrap();
        assert_eq!(r, TextRef { path: dir.path().join(p.file_name()), mime: "text/plain".into(), chars: 5000 });
        assert_eq!(std::fs::read_to_string(&r.path).unwrap(), p.text.as_ref());
        assert_eq!(std::fs::metadata(&r.path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(save(&Paste::new("é".repeat(5000)), dir.path()).unwrap().path, r.path);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
