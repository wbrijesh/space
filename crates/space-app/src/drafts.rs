//! Drafts: what each thread's composer holds (its text, pictures, long pastes, attached files
//! and quote), kept across restarts. They're in `drafts.json` next to the settings file, by
//! thread id, and their pictures in `drafts/` beside it, named by their sha256 like sent ones (a
//! picture no draft uses any more is deleted). Written at most every half second while a draft
//! changes (each render checks), and at once when you switch threads, send, or quit; a sent (or
//! queued) message leaves its thread's draft empty, so it's gone from the file too.
//!
//! A saved thread's draft waits here until the thread is made (listed at launch, or a done one
//! opened to read), and comes back here when it goes again (a done thread you leave). Of new
//! threads (nothing sent: not saved, not listed) only the one shown is kept, with its folder:
//! Space opens on it next time.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{App, Context, Window};
use serde::{Deserialize, Serialize};
use space_core::session::ImageRef;

use crate::app::SpaceApp;
use crate::attach::DraftImage;
use crate::images::Prepared;
use crate::paste::Paste;
use crate::threads::Thread;

/// How often drafts are written while they change.
const SAVE_EVERY: Duration = Duration::from_millis(500);

/// One thread's unsent message.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    /// Pictures, in the drafts' pictures folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageRef>,
    /// Long pastes, their text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pastes: Vec<String>,
    /// Attached files and folders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<PathBuf>,
    /// A new thread's (nothing sent, so it isn't saved): its folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_in: Option<PathBuf>,
}

impl Draft {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.quote.is_none() && self.images.is_empty() && self.pastes.is_empty() && self.attachments.is_empty()
    }
}

/// `drafts.json`.
#[derive(Default, Serialize, Deserialize)]
struct DraftsFile {
    #[serde(default)]
    threads: BTreeMap<String, Draft>,
}

/// The drafts file, and the drafts of threads that aren't made (see the module docs).
#[derive(Default)]
pub struct Drafts {
    /// `drafts.json` (`None`: drafts aren't kept).
    path: Option<PathBuf>,
    kept: HashMap<String, Draft>,
    /// What was written last.
    written: Option<String>,
    /// A write is due.
    scheduled: bool,
}

impl Drafts {
    pub fn load(path: Option<PathBuf>) -> Self {
        let file = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<DraftsFile>(&s).map_err(|e| tracing::warn!("drafts: {e}")).ok())
            .unwrap_or_default();
        Self { path, kept: file.threads.into_iter().collect(), written: None, scheduled: false }
    }

    /// Where the drafts' pictures are kept.
    fn pictures(&self) -> Option<PathBuf> {
        Some(self.path.as_ref()?.with_file_name("drafts"))
    }

    /// Thread `id`'s draft, now that the thread is made.
    pub fn take(&mut self, id: &str) -> Option<Draft> {
        self.kept.remove(id)
    }

    /// The new thread's draft that was shown last, with its thread id.
    pub fn take_new(&mut self) -> Option<(String, Draft)> {
        let id = self.kept.iter().find(|(_, d)| d.new_in.is_some())?.0.clone();
        self.kept.remove_entry(&id)
    }

    /// Keep thread `id`'s draft while the thread isn't made.
    pub fn keep(&mut self, id: &str, draft: Draft) {
        if draft.is_empty() {
            self.kept.remove(id);
        } else {
            self.kept.insert(id.to_string(), draft);
        }
    }

    pub fn forget(&mut self, id: &str) {
        self.kept.remove(id);
    }

    /// Keep only the drafts of these threads (the saved ones).
    pub fn keep_only(&mut self, ids: &HashSet<&str>) {
        self.kept.retain(|id, _| ids.contains(id.as_str()));
    }

    /// Write `drafts` (and `pictures`, the prepared images they name) unless that's what was
    /// written last; pictures no draft names any more go.
    fn write(&mut self, drafts: BTreeMap<String, Draft>, pictures: &[Prepared]) -> std::io::Result<()> {
        let (Some(path), Some(dir)) = (self.path.clone(), self.pictures()) else { return Ok(()) };
        let file = DraftsFile { threads: drafts };
        let json = serde_json::to_string_pretty(&file)?;
        // Nothing to keep, and no file to empty.
        if self.written.as_deref() == Some(json.as_str()) || (self.written.is_none() && file.threads.is_empty() && !path.exists()) {
            self.written = Some(json);
            return Ok(());
        }
        for p in pictures {
            crate::images::save(p, &dir)?;
        }
        write_atomic(&path, json.as_bytes())?;
        let used: HashSet<_> = file.threads.values().flat_map(|d| &d.images).filter_map(|i| i.path.file_name()).collect();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if !used.contains(entry.file_name().as_os_str()) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        self.written = Some(json);
        Ok(())
    }
}

/// Replace `path` with `bytes`: a temp file beside it, synced, then renamed; owner-only.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// What thread `t`'s composer holds, `text` being its text, with its pictures named in `dir`
/// (those still being prepared are left out), and the pictures.
pub fn draft_of(t: &Thread, text: &str, dir: &Path) -> (Draft, Vec<Prepared>) {
    let pictures: Vec<Prepared> = t.images.iter().filter_map(|i| i.prepared.clone()).collect();
    let draft = Draft {
        text: text.to_string(),
        quote: t.quote.clone(),
        images: pictures
            .iter()
            .map(|p| ImageRef { path: dir.join(p.file_name()), mime: p.mime.to_string(), width: p.width, height: p.height, name: p.name.clone() })
            .collect(),
        pastes: t.pastes.iter().map(|p| p.text.to_string()).collect(),
        attachments: t.attachments.clone(),
        new_in: t.is_empty().then(|| t.cwd.clone()),
    };
    (draft, pictures)
}

/// Put `draft` in thread `t`'s composer (pictures that are gone are left out).
pub fn restore(t: &mut Thread, draft: Draft, next_image_id: &mut u64) {
    t.draft = draft.text;
    t.quote = draft.quote;
    t.images = draft
        .images
        .iter()
        .filter_map(crate::images::load_saved)
        .map(|p| {
            *next_image_id += 1;
            DraftImage::ready(*next_image_id - 1, p)
        })
        .collect();
    t.pastes = draft.pastes.into_iter().map(Paste::new).collect();
    t.attachments = draft.attachments;
}

impl SpaceApp {
    /// What thread `t`'s composer holds: the shown thread's text is the input's.
    fn thread_draft(&self, t: &Thread, dir: &Path, cx: &App) -> (Draft, Vec<Prepared>) {
        if self.selected == Some(t.key) {
            draft_of(t, &self.input.read(cx).value(), dir)
        } else {
            draft_of(t, &t.draft, dir)
        }
    }

    /// Write the drafts now (if they changed).
    pub(crate) fn save_drafts(&mut self, cx: &App) {
        let Some(dir) = self.drafts.pictures() else { return };
        let mut drafts: BTreeMap<String, Draft> = self.drafts.kept.iter().map(|(id, d)| (id.clone(), d.clone())).collect();
        let mut pictures = Vec::new();
        for t in &self.threads {
            // Of new threads, only the one shown.
            if t.is_empty() && self.selected != Some(t.key) {
                continue;
            }
            let (draft, mut p) = self.thread_draft(t, &dir, cx);
            if !draft.is_empty() {
                drafts.insert(t.id.clone(), draft);
                pictures.append(&mut p);
            }
        }
        if let Err(e) = self.drafts.write(drafts, &pictures) {
            tracing::warn!("saving drafts: {e}");
        }
    }

    /// A render: write the drafts within [`SAVE_EVERY`] if they changed.
    pub(crate) fn save_drafts_soon(&mut self, cx: &mut Context<Self>) {
        if self.drafts.scheduled || self.drafts.path.is_none() {
            return;
        }
        self.drafts.scheduled = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_EVERY).await;
            let _ = this.update(cx, |this, cx| {
                this.drafts.scheduled = false;
                this.save_drafts(cx);
            });
        })
        .detach();
    }

    /// Thread `t` is going (a done thread you leave, or one marked done): a saved thread's draft
    /// waits for it to be opened again.
    pub(crate) fn keep_draft(&mut self, t: &Thread, cx: &App) {
        let Some(dir) = self.drafts.pictures() else { return };
        if t.is_empty() {
            return;
        }
        let (draft, pictures) = self.thread_draft(t, &dir, cx);
        for p in &pictures {
            if let Err(e) = crate::images::save(p, &dir) {
                tracing::warn!("saving a draft's picture: {e}");
            }
        }
        self.drafts.keep(&t.id, draft);
    }

    /// At launch, opened with just a new thread: it's the new thread that was shown when Space
    /// quit, if that one had a draft (in its folder, if that's still there).
    pub(crate) fn open_new_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let Some((id, draft)) = self.drafts.take_new() else { return };
        let dir = draft.new_in.clone().filter(|d| d.is_dir()).unwrap_or_else(|| self.project_dir.clone());
        let mut thread = Thread::new(key, dir);
        thread.id = id;
        restore(&mut thread, draft, &mut self.next_image_id);
        let text = thread.draft.clone();
        if let Some(t) = self.thread_mut(key) {
            *t = thread;
        }
        self.input.update(cx, |s, cx| s.set_value(text, window, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(bytes: &[u8]) -> Prepared {
        use sha2::{Digest as _, Sha256};
        let sha256 = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
        Prepared { bytes: bytes.into(), mime: "image/png", width: 3, height: 2, sha256, name: Some("a.png".into()) }
    }

    #[test]
    fn a_draft_comes_back_whole() {
        let dir = tempfile::tempdir().unwrap();
        let pictures = dir.path().join("drafts");
        let mut t = Thread::new(1, dir.path().to_path_buf());
        t.quote = Some("quoted".into());
        t.images.push(DraftImage::ready(0, picture(b"one")));
        // Still being prepared: left out.
        t.images.push(DraftImage { id: 1, prepared: None, pic: None });
        t.pastes.push(Paste::new("long"));
        t.attachments.push("/tmp/x.rs".into());
        let (draft, p) = draft_of(&t, "hello", &pictures);
        assert_eq!(draft.new_in.as_deref(), Some(dir.path()), "a new thread's draft names its folder");
        assert_eq!(p.len(), 1);

        let mut drafts = Drafts::load(Some(dir.path().join("drafts.json")));
        drafts.write(BTreeMap::from([(t.id.clone(), draft.clone())]), &p).unwrap();
        let mut back = Drafts::load(Some(dir.path().join("drafts.json")));
        let (id, read) = back.take_new().unwrap();
        assert_eq!((id, &read), (t.id.clone(), &draft));

        let mut u = Thread::new(2, dir.path().to_path_buf());
        let mut next = 7;
        restore(&mut u, read, &mut next);
        assert_eq!((u.draft.as_str(), u.quote.as_deref()), ("hello", Some("quoted")));
        assert_eq!(u.pastes[0].text.as_ref(), "long");
        assert_eq!(u.attachments, t.attachments);
        assert_eq!((u.images.len(), u.images[0].id, next), (1, 7, 8));
        let restored = u.images[0].prepared.as_ref().unwrap();
        assert_eq!((&*restored.bytes, restored.sha256.as_str()), (&b"one"[..], t.images[0].prepared.as_ref().unwrap().sha256.as_str()));
    }

    #[test]
    fn pictures_no_draft_uses_go() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drafts.json");
        let mut drafts = Drafts::load(Some(path.clone()));
        let (a, b) = (picture(b"a"), picture(b"b"));
        let pictures = dir.path().join("drafts");
        let named = |p: &Prepared| Draft {
            images: vec![ImageRef { path: pictures.join(p.file_name()), mime: "image/png".into(), width: 1, height: 1, name: None }],
            ..Default::default()
        };
        drafts.write(BTreeMap::from([("x".into(), named(&a)), ("y".into(), named(&b))]), &[a.clone(), b.clone()]).unwrap();
        assert_eq!(std::fs::read_dir(&pictures).unwrap().count(), 2);
        // Sent from "y": its picture goes; nothing is written when nothing changed.
        drafts.write(BTreeMap::from([("x".into(), named(&a))]), std::slice::from_ref(&a)).unwrap();
        assert!(pictures.join(a.file_name()).exists() && !pictures.join(b.file_name()).exists());
        std::fs::remove_file(&path).unwrap();
        drafts.write(BTreeMap::from([("x".into(), named(&a))]), &[a]).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn drafts_read_from_older_and_newer_files() {
        // Fields it doesn't know are ignored, missing ones are empty; empty ones aren't written.
        let file: DraftsFile = serde_json::from_str(r#"{"threads": {"a": {"text": "hi", "later": 1}}, "other": true}"#).unwrap();
        assert_eq!(file.threads["a"], Draft { text: "hi".into(), ..Default::default() });
        assert_eq!(serde_json::to_value(&file.threads["a"]).unwrap(), serde_json::json!({"text": "hi"}));
        assert!(serde_json::from_str::<DraftsFile>("{}").unwrap().threads.is_empty());
        assert!(Draft { text: " \n".into(), ..Default::default() }.is_empty());
    }

    #[test]
    fn kept_drafts_are_by_thread() {
        let mut drafts = Drafts::default();
        drafts.keep("a", Draft { text: "x".into(), ..Default::default() });
        drafts.keep("b", Draft { text: "y".into(), ..Default::default() });
        drafts.keep("a", Draft::default());
        assert!(drafts.take("a").is_none(), "an empty draft isn't kept");
        drafts.keep_only(&HashSet::from(["c"]));
        assert!(drafts.take("b").is_none(), "a thread that's gone takes its draft with it");
    }
}
