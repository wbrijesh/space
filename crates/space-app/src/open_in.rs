//! Opening a folder (or a file at a line) in another app: which known apps are installed, their
//! icons, and opening. The title bar's "Open in" uses it for the thread's folder.

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{AppContext as _, ClipboardItem, Context, Window};
use objc2_app_kit::NSWorkspace;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImageAlphaInfo, kCGColorSpaceSRGB};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::app::SpaceApp;

/// What an app is for; the menu lists editors, then Finder, then terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Editor,
    Finder,
    Terminal,
}

/// An app Space knows how to open a folder in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Known {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: Kind,
    /// A URL scheme that opens a file at a line (`<scheme>://file/<path>:<line>`).
    pub line_scheme: Option<&'static str>,
}

const fn app(id: &'static str, name: &'static str, kind: Kind, line_scheme: Option<&'static str>) -> Known {
    Known { id, name, kind, line_scheme }
}

/// The apps looked for, in menu order within each kind.
pub const KNOWN: &[Known] = &[
    app("com.microsoft.VSCode", "VS Code", Kind::Editor, Some("vscode")),
    app("com.microsoft.VSCodeInsiders", "VS Code Insiders", Kind::Editor, Some("vscode-insiders")),
    app("com.vscodium", "VSCodium", Kind::Editor, Some("vscodium")),
    app("com.vscodium.VSCodiumInsiders", "VSCodium Insiders", Kind::Editor, Some("vscodium-insiders")),
    app("com.todesktop.230313mzl4w4u92", "Cursor", Kind::Editor, Some("cursor")),
    app("com.exafunction.windsurf", "Windsurf", Kind::Editor, Some("windsurf")),
    app("dev.zed.Zed", "Zed", Kind::Editor, Some("zed")),
    app("dev.zed.Zed-Preview", "Zed Preview", Kind::Editor, Some("zed")),
    app("com.sublimetext.4", "Sublime Text", Kind::Editor, None),
    app("com.panic.Nova", "Nova", Kind::Editor, None),
    app("com.apple.dt.Xcode", "Xcode", Kind::Editor, None),
    app("com.jetbrains.intellij", "IntelliJ IDEA", Kind::Editor, None),
    app("com.jetbrains.intellij.ce", "IntelliJ IDEA CE", Kind::Editor, None),
    app("com.jetbrains.rustrover", "RustRover", Kind::Editor, None),
    app("com.jetbrains.WebStorm", "WebStorm", Kind::Editor, None),
    app("com.jetbrains.pycharm", "PyCharm", Kind::Editor, None),
    app("com.jetbrains.pycharm.ce", "PyCharm CE", Kind::Editor, None),
    app("com.jetbrains.goland", "GoLand", Kind::Editor, None),
    app(FINDER, "Finder", Kind::Finder, None),
    app("com.apple.Terminal", "Terminal", Kind::Terminal, None),
    app("com.googlecode.iterm2", "iTerm2", Kind::Terminal, None),
    app("com.mitchellh.ghostty", "Ghostty", Kind::Terminal, None),
    app("dev.warp.Warp-Stable", "Warp", Kind::Terminal, None),
    app("net.kovidgoyal.kitty", "kitty", Kind::Terminal, None),
    app("com.github.wez.wezterm", "WezTerm", Kind::Terminal, None),
    app("org.alacritty", "Alacritty", Kind::Terminal, None),
];

pub const FINDER: &str = "com.apple.finder";

/// An installed app: what it is and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub known: Known,
    pub path: PathBuf,
}

/// The known apps installed now, editors first, then Finder, then terminals.
pub fn detect() -> Vec<Installed> {
    let workspace = NSWorkspace::sharedWorkspace();
    let mut found: Vec<Installed> = KNOWN
        .iter()
        .filter_map(|k| {
            let url = workspace.URLForApplicationWithBundleIdentifier(&NSString::from_str(k.id))?;
            let path = url.path()?.to_string();
            Some(Installed { known: *k, path: path.into() })
        })
        .collect();
    found.sort_by_key(|a| a.known.kind);
    found
}

/// The app to use: `saved` (a bundle id) if it's installed, else Finder.
pub fn choose<'a>(apps: &'a [Installed], saved: Option<&str>) -> Option<&'a Installed> {
    saved.and_then(|id| apps.iter().find(|a| a.known.id == id)).or_else(|| apps.iter().find(|a| a.known.id == FINDER))
}

/// Icon side in pixels (16pt at 2x, drawn at twice that for sharpness).
const ICON_PX: usize = 64;

/// The app's icon as a PNG.
pub fn icon_png(app: &Path) -> Option<Vec<u8>> {
    let image = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(&app.to_string_lossy()));
    let side = ICON_PX as f64;
    let mut rect = NSRect::new(NSPoint::new(0., 0.), NSSize::new(side, side));
    // SAFETY: a valid rect pointer, no context or hints.
    let cg = unsafe { image.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    let mut pixels = vec![0u8; ICON_PX * ICON_PX * 4];
    let info = CGImageAlphaInfo::PremultipliedLast.0;
    // SAFETY: the buffer is ICON_PX² RGBA bytes and outlives the context.
    let context = unsafe { CGBitmapContextCreate(pixels.as_mut_ptr().cast::<c_void>(), ICON_PX, ICON_PX, 8, ICON_PX * 4, Some(&space), info) }?;
    CGContext::draw_image(Some(&context), CGRect::new(CGPoint::new(0., 0.), CGSize::new(side, side)), Some(&cg));
    let out = CGBitmapContextCreateImage(Some(&context))?;
    drop(context);
    crate::images::encode(&out, true)
}

/// Icons drawn so far, by bundle id.
#[derive(Default)]
pub struct Icons(HashMap<&'static str, Option<Arc<gpui::Image>>>);

impl Icons {
    pub fn get(&self, id: &str) -> Option<Arc<gpui::Image>> {
        self.0.get(id).cloned().flatten()
    }

    /// The apps whose icons aren't drawn yet.
    pub fn missing<'a>(&self, apps: &'a [Installed]) -> Vec<&'a Installed> {
        apps.iter().filter(|a| !self.0.contains_key(a.known.id)).collect()
    }

    pub fn insert(&mut self, id: &'static str, png: Option<Vec<u8>>) {
        self.0.insert(id, png.map(|bytes| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes))));
    }
}

/// Open `path` in `app`: a file at `line` where the app has a URL for that, else the path.
/// Blocks until `open` says whether it worked; the error is a short sentence.
///
/// A file can't open in Finder or a terminal: Finder reveals it, a terminal opens at its folder.
pub fn open(path: &Path, line: Option<u32>, app: &Known) -> Result<(), String> {
    let mut cmd = std::process::Command::new("/usr/bin/open");
    let file = path.is_file();
    match (app.kind, line, app.line_scheme) {
        (Kind::Finder, ..) if file => cmd.arg("-R").arg(path),
        (Kind::Terminal, ..) if file => cmd.args(["-b", app.id]).arg(path.parent().unwrap_or(path)),
        (_, Some(line), Some(scheme)) if file => cmd.arg(format!("{scheme}://file{}:{line}", path.display())),
        _ => cmd.args(["-b", app.id]).arg(path),
    };
    match cmd.output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(format!("Couldn't open {} in {}: {}", path.display(), app.name, err.lines().next().unwrap_or("it failed").trim()))
        }
        Err(e) => Err(format!("Couldn't open {} in {}: {e}", path.display(), app.name)),
    }
}

// ----- the title bar's "Open in" ------------------------------------------------------------

impl SpaceApp {
    /// Look for the known apps again (at launch, and when the menu opens, so new installs
    /// show), and draw the icons not drawn yet. Off the UI thread.
    pub(crate) fn refresh_open_in(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let apps = cx.background_spawn(async { detect() }).await;
            let Ok(missing) = this.update(cx, |this, cx| {
                let missing: Vec<Installed> = this.open_in_icons.missing(&apps).into_iter().cloned().collect();
                if this.open_in_apps != apps {
                    this.open_in_apps = apps;
                    cx.notify();
                }
                missing
            }) else {
                return;
            };
            if missing.is_empty() {
                return;
            }
            let icons = cx.background_spawn(async move { missing.into_iter().map(|a| (a.known.id, icon_png(&a.path))).collect::<Vec<_>>() }).await;
            let _ = this.update(cx, |this, cx| {
                for (id, png) in icons {
                    this.open_in_icons.insert(id, png);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The app "Open in" uses now.
    pub(crate) fn open_in_app(&self) -> Option<&Installed> {
        choose(&self.open_in_apps, self.settings.settings.open_in.as_deref())
    }

    /// Open the shown thread's folder in `id` (`None`: the app used last), and remember it.
    pub(crate) fn open_thread_folder(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        self.open_in_menu = false;
        let Some(dir) = self.current().map(|t| t.cwd.clone()) else { return };
        let app = match id {
            Some(id) => self.open_in_apps.iter().find(|a| a.known.id == id),
            None => self.open_in_app(),
        };
        let Some(app) = app.map(|a| a.known) else { return };
        if id.is_some() {
            self.settings.update(|s| s.open_in = Some(app.id.to_string()));
        }
        cx.spawn(async move |this, cx| {
            if let Err(e) = cx.background_spawn(async move { open(&dir, None, &app) }).await {
                let _ = this.update(cx, |this, cx| {
                    this.banner = Some(e);
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn copy_thread_path(&mut self, cx: &mut Context<Self>) {
        self.open_in_menu = false;
        if let Some(dir) = self.current().map(|t| t.cwd.clone()) {
            cx.write_to_clipboard(ClipboardItem::new_string(dir.display().to_string()));
        }
        cx.notify();
    }

    pub(crate) fn show_open_in_menu(&mut self, open: bool, _: &mut Window, cx: &mut Context<Self>) {
        self.open_in_menu = open;
        if open {
            self.refresh_open_in(cx);
        }
        cx.notify();
    }

    /// The control's data for the title bar (`None` without apps to offer).
    pub(crate) fn open_in_view(&self, cx: &mut Context<Self>) -> Option<space_ui::components::open_in::OpenIn> {
        use std::rc::Rc;

        use space_ui::components::open_in::{OpenIn, OpenInApp};
        let item = |a: &Installed| OpenInApp {
            id: a.known.id.into(),
            name: a.known.name.into(),
            icon: self.open_in_icons.get(a.known.id).map(Into::into),
            group: a.known.kind as u8,
        };
        let current = item(self.open_in_app()?);
        let this = cx.entity().downgrade();
        let (t1, t2, t3, t4) = (this.clone(), this.clone(), this.clone(), this);
        Some(OpenIn {
            current,
            apps: self.open_in_apps.iter().map(item).collect(),
            menu_open: self.open_in_menu,
            on_open: Rc::new(move |_, cx| {
                let _ = t1.update(cx, |this, cx| this.open_thread_folder(None, cx));
            }),
            on_menu: Rc::new(move |open, w, cx| {
                let _ = t2.update(cx, |this, cx| this.show_open_in_menu(open, w, cx));
            }),
            on_pick: Rc::new(move |id, _, cx| {
                let _ = t3.update(cx, |this, cx| this.open_thread_folder(Some(&id), cx));
            }),
            on_copy_path: Rc::new(move |_, cx| {
                let _ = t4.update(cx, |this, cx| this.copy_thread_path(cx));
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(id: &str) -> Installed {
        Installed { known: *KNOWN.iter().find(|k| k.id == id).unwrap(), path: PathBuf::from("/Applications/x.app") }
    }

    #[test]
    fn the_saved_app_or_finder() {
        let apps = [installed("dev.zed.Zed"), installed(FINDER)];
        assert_eq!(choose(&apps, Some("dev.zed.Zed")).unwrap().known.id, "dev.zed.Zed");
        // Saved but uninstalled since, or never saved: Finder.
        assert_eq!(choose(&apps, Some("com.panic.Nova")).unwrap().known.id, FINDER);
        assert_eq!(choose(&apps, None).unwrap().known.id, FINDER);
    }

    #[test]
    fn known_ids_are_unique_and_grouped() {
        let mut ids: Vec<_> = KNOWN.iter().map(|k| k.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), KNOWN.len());
        assert!(KNOWN.windows(2).all(|w| w[0].kind <= w[1].kind), "listed editors, Finder, terminals");
    }

    #[test]
    fn a_failed_open_says_why() {
        let missing = Known { id: "dev.space.no-such-app", name: "Nothing", kind: Kind::Editor, line_scheme: None };
        let err = open(Path::new("/tmp"), None, &missing).unwrap_err();
        assert!(err.starts_with("Couldn't open /tmp in Nothing: "), "{err}");
    }

    #[test]
    fn finder_is_found_and_has_an_icon() {
        let apps = detect();
        let finder = apps.iter().find(|a| a.known.id == FINDER).expect("Finder is always installed");
        let png = icon_png(&finder.path).expect("an icon");
        assert!(png.starts_with(b"\x89PNG"));
    }
}
