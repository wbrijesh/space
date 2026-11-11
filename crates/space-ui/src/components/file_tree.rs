//! The Files view's tree: one row per folder or file (a folder holding only a folder is one
//! row, `src/main`), git status as color and a letter, a dot on folders with changes inside,
//! a right-click menu, and the filter field above it. Virtualized: only the rows on screen are
//! built.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::input::{Input, InputState};
use gpui::component::{Icon, IconName, Sizable as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::menu::{ContextItem, context_menu};
use crate::theme::{color, radius, size, text};

/// A file's change in git.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GitMark {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflict,
}

impl GitMark {
    pub fn letter(self) -> &'static str {
        match self {
            GitMark::Modified => "M",
            GitMark::Added => "A",
            GitMark::Deleted => "D",
            GitMark::Renamed => "R",
            GitMark::Untracked => "U",
            GitMark::Conflict => "!",
        }
    }

    pub fn color(self) -> Hsla {
        match self {
            GitMark::Modified => color::yellow(),
            GitMark::Added | GitMark::Untracked => color::green(),
            GitMark::Deleted | GitMark::Conflict => color::red(),
            GitMark::Renamed => color::blue(),
        }
    }
}

/// One row as shown.
#[derive(Debug, Clone, PartialEq)]
pub struct FileRow {
    pub path: PathBuf,
    /// The name, or a chain of folders (`src/main`).
    pub name: SharedString,
    pub depth: usize,
    pub dir: bool,
    pub expanded: bool,
    pub git: Option<GitMark>,
    /// A folder with changes somewhere inside.
    pub changes_inside: bool,
    pub selected: bool,
}

/// What a row's menu asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAction {
    /// Show it in a tab.
    Open,
    /// In the "Open in" app.
    OpenIn,
    Reveal,
    CopyPath,
    CopyRelativePath,
}

/// A key the tree acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeKey {
    Up,
    Down,
    /// Close the folder, or go to the parent.
    Left,
    /// Open the folder, or go into it.
    Right,
    /// Open the file, or open/close the folder.
    Enter,
}

pub type RowHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
pub type ActionHandler = Rc<dyn Fn(usize, RowAction, &mut Window, &mut App)>;
pub type KeyHandler = Rc<dyn Fn(TreeKey, &mut Window, &mut App)>;

#[derive(Clone)]
pub struct TreeHandlers {
    /// A row was clicked: a folder opens or closes, a file shows.
    pub on_click: RowHandler,
    pub on_action: ActionHandler,
    pub on_key: KeyHandler,
}

impl Default for TreeHandlers {
    fn default() -> Self {
        Self { on_click: Rc::new(|_, _, _| {}), on_action: Rc::new(|_, _, _, _| {}), on_key: Rc::new(|_, _, _| {}) }
    }
}

/// A row's height and each level's indent.
pub const ROW_H: Pixels = px(26.);
const INDENT: f32 = 14.;

/// The icon for a file, by its name.
pub fn file_icon(name: &str) -> Lucide {
    let lower = name.to_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "py" | "go" | "c" | "h" | "cc" | "cpp" | "hpp" | "swift" | "m" | "java" | "kt" | "rb" | "php"
        | "cs" | "zig" | "lua" | "ex" | "exs" | "hs" | "scala" | "dart" | "vue" | "svelte" | "html" | "css" | "scss" | "sql" => Lucide::FileCode,
        "json" | "jsonc" | "json5" | "toml" | "yaml" | "yml" | "xml" | "plist" => Lucide::FileBraces,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "icns" | "bmp" | "tif" | "tiff" | "heic" => Lucide::FileImage,
        "lock" => Lucide::FileLock,
        "sh" | "bash" | "zsh" | "fish" => Lucide::FileTerminal,
        "zip" | "gz" | "tgz" | "tar" | "xz" | "bz2" | "7z" | "dmg" => Lucide::FileArchive,
        "md" | "mdx" | "txt" | "rst" => Lucide::FileText,
        _ if lower.starts_with('.') || lower == "justfile" || lower == "makefile" || lower == "dockerfile" => Lucide::FileCog,
        _ => Lucide::File,
    }
}

fn row(ix: usize, r: &FileRow, open_label: &SharedString, h: &TreeHandlers, window: &mut Window, cx: &mut App) -> AnyElement {
    let name_color = match (r.git, r.dir) {
        (Some(mark), false) => mark.color(),
        (_, true) => color::text(),
        _ => color::text_2(),
    };
    let lead = if r.dir {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(3.))
            .text_color(color::text_3())
            .child(Icon::new(if r.expanded { IconName::ChevronDown } else { IconName::ChevronRight }).size(size::ICON_SM))
            .child(Icon::new(if r.expanded { Lucide::FolderOpen } else { Lucide::Folder }).size(size::ICON).text_color(color::text_2()))
    } else {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(3.))
            .child(div().size(size::ICON_SM))
            .child(Icon::new(file_icon(&r.name)).size(size::ICON).text_color(color::text_3()))
    };
    let mark = match (r.git, r.dir && r.changes_inside) {
        (Some(m), false) if !r.dir => {
            Some(div().w(px(12.)).flex_none().font_family(text::MONO_FONT).text_size(text::XS).text_color(m.color()).child(m.letter()).into_any_element())
        }
        (_, true) => Some(div().w(px(12.)).flex_none().flex().justify_center().child(div().size(px(5.)).rounded_full().bg(color::yellow().opacity(0.7))).into_any_element()),
        _ => None,
    };
    let click = h.on_click.clone();
    let el = div()
        .id(("file-row", ix))
        .w_full()
        .h(ROW_H)
        .pl(px(6. + INDENT * r.depth as f32))
        .pr_2()
        .flex()
        .items_center()
        .gap_1p5()
        .rounded(radius::SM)
        .text_size(text::SM)
        .cursor_pointer()
        .map(|d| if r.selected { d.bg(color::pressed()) } else { d.hover(|s| s.bg(color::hover())) })
        .child(lead)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(name_color)
                .when(r.git == Some(GitMark::Deleted), |d| d.line_through())
                .child(r.name.clone()),
        )
        .children(mark)
        .on_click(move |_, w, cx| click(ix, w, cx));
    let item = |label: SharedString, action: RowAction| {
        let act = h.on_action.clone();
        ContextItem::new(label, move |w, cx| act(ix, action, w, cx))
    };
    let items = vec![
        item("Open".into(), RowAction::Open),
        item(open_label.clone(), RowAction::OpenIn),
        item("Reveal in Finder".into(), RowAction::Reveal),
        item("Copy Path".into(), RowAction::CopyPath),
        item("Copy Relative Path".into(), RowAction::CopyRelativePath),
    ];
    context_menu(el, format!("file-row-{}", r.path.display()), items, window, cx).into_any_element()
}

/// The tree: `rows` in a virtualized list that takes arrow keys while focused. `open_label` is
/// the menu's first item ("Open in Zed").
pub fn file_tree(rows: Rc<Vec<FileRow>>, open_label: SharedString, scroll: &UniformListScrollHandle, focus: &FocusHandle, h: TreeHandlers) -> Stateful<Div> {
    let key = h.on_key.clone();
    let count = rows.len();
    let list = uniform_list("file-tree-rows", count, move |range, window, cx| {
        range.filter_map(|ix| rows.get(ix).map(|r| row(ix, r, &open_label, &h, window, cx))).collect()
    })
    .track_scroll(scroll)
    .p_1()
    .size_full();
    div()
        .id("file-tree")
        .key_context("FileTree")
        .track_focus(focus)
        .flex_1()
        .min_h_0()
        .on_key_down(move |e, w, cx| {
            let k = match e.keystroke.key.as_str() {
                "up" => TreeKey::Up,
                "down" => TreeKey::Down,
                "left" => TreeKey::Left,
                "right" => TreeKey::Right,
                "enter" => TreeKey::Enter,
                _ => return,
            };
            if e.keystroke.modifiers.modified() {
                return;
            }
            cx.stop_propagation();
            key(k, w, cx);
        })
        .child(list)
}

/// The filter field, in the pane's header.
pub fn filter_field(input: &Entity<InputState>) -> Div {
    div().flex_1().min_w_0().h(px(24.)).px_1().flex().items_center().rounded(radius::MD).bg(color::surface()).border_1().border_color(color::border()).child(
        Input::new(input)
            .small()
            .appearance(false)
            .prefix(Icon::new(IconName::Search).size(size::ICON_SM).text_color(color::text_3())),
    )
}

/// Instead of rows: still listing, an error, or no matches.
pub fn notice(message: impl Into<SharedString>) -> Div {
    div().flex_1().p_4().text_size(text::SM).text_color(color::text_3()).child(message.into())
}

#[cfg(feature = "dev")]
pub fn fixture_rows() -> Vec<FileRow> {
    let r = |depth: usize, name: &str, dir: bool, expanded: bool, git: Option<GitMark>, inside: bool| FileRow {
        path: PathBuf::from(format!("/repo/{name}")),
        name: name.to_string().into(),
        depth,
        dir,
        expanded,
        git,
        changes_inside: inside,
        selected: false,
    };
    let mut rows = vec![
        r(0, ".github/workflows", true, false, None, false),
        r(0, "crates", true, true, None, true),
        r(1, "space-app", true, true, None, true),
        r(2, "src", true, true, None, true),
        r(3, "files.rs", false, false, Some(GitMark::Untracked), false),
        r(3, "main.rs", false, false, Some(GitMark::Modified), false),
        r(3, "old_tree.rs", false, false, Some(GitMark::Deleted), false),
        r(3, "view.rs", false, false, None, false),
        r(2, "Cargo.toml", false, false, Some(GitMark::Modified), false),
        r(1, "space-ui", true, false, None, false),
        r(0, "docs", true, false, None, false),
        r(0, ".gitignore", false, false, None, false),
        r(0, "Cargo.lock", false, false, None, false),
        r(0, "justfile", false, false, None, false),
        r(0, "logo.png", false, false, Some(GitMark::Added), false),
        r(0, "README.md", false, false, Some(GitMark::Renamed), false),
    ];
    rows[7].selected = true;
    rows
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    vec![ComponentPreview::new(Area::Shell, "File tree", "The Files view: folders, files, git status, and the filter.").stacked().examples([
        Example::new("tree", |window, cx| {
            let scroll = window.use_keyed_state("file-tree-preview-scroll", cx, |_, _| UniformListScrollHandle::new()).read(cx).clone();
            let focus = window.use_keyed_state("file-tree-preview-focus", cx, |_, cx| cx.focus_handle()).read(cx).clone();
            let input = window.use_keyed_state("file-tree-preview-input", cx, |window, cx| InputState::new(window, cx).placeholder("Filter files"));
            div()
                .w(px(320.))
                .h(px(480.))
                .flex()
                .flex_col()
                .bg(color::bg())
                .border_1()
                .border_color(color::border())
                .child(div().p_2().flex().child(filter_field(&input)))
                .child(file_tree(Rc::new(fixture_rows()), "Open in Zed".into(), &scroll, &focus, TreeHandlers::default()))
                .into_any_element()
        }),
        Example::new("no matches", |_, _| {
            div().w(px(320.)).h(px(80.)).flex().bg(color::bg()).border_1().border_color(color::border()).child(notice("No files match.")).into_any_element()
        }),
    ])]
}
