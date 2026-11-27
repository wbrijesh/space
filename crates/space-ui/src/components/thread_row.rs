//! Sidebar thread row: one line with the thread title and an activity indicator; on hover an
//! archive button marks it done. Right-click for Pin, Rename Thread… and Mark as Done. A done
//! thread's row (under the Done header) is dimmer: a click opens it to read, and on hover a
//! restore button reopens it.

use gpui::assets::IconName;
use gpui::component::Icon;
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::loaders::Dots;
use super::menu::{ContextItem, context_menu};
use super::tip::TipExt as _;
use super::widgets::{icon_button, status_dot};
use crate::ClickHandler;
use crate::actions::{MarkDone, RenameThread, TogglePinThread};
use crate::theme::{color, radius, size, text};

/// What the right edge of a row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowIndicator {
    None,
    /// A turn is in progress (thinking, streaming, tools): three-dot pulse.
    Working,
    /// Waiting for a permission answer: yellow dot.
    NeedsInput,
    /// A tool call waits for the guardrail (fail closed): yellow pause.
    Paused,
    /// New reply since last viewed: blue dot.
    Unread,
    /// New error since last viewed: red dot.
    UnreadError,
}

impl RowIndicator {
    /// Priority: paused > waiting for input > working > unseen news. The selected row hides
    /// dots (you are looking at it) but keeps showing work in progress, and a pause.
    pub fn resolve(selected: bool, working: bool, needs_input: bool, unread: bool, failed: bool) -> Self {
        Self::resolve_paused(false, selected, working, needs_input, unread, failed)
    }

    pub fn resolve_paused(paused: bool, selected: bool, working: bool, needs_input: bool, unread: bool, failed: bool) -> Self {
        match () {
            _ if paused => Self::Paused,
            _ if needs_input && !selected => Self::NeedsInput,
            _ if working => Self::Working,
            _ if unread && !selected && failed => Self::UnreadError,
            _ if unread && !selected => Self::Unread,
            _ => Self::None,
        }
    }
}

#[derive(IntoElement)]
pub struct ThreadRow {
    id: ElementId,
    title: SharedString,
    selected: bool,
    /// A done thread, listed under Done: its title dimmer than open ones'.
    done: bool,
    indicator: RowIndicator,
    /// Preview only: paint the hover state without a pointer.
    force_hover: bool,
    /// Something runs in one of its terminals (or has new output): a terminal mark when
    /// nothing else shows.
    terminal: bool,
    on_click: Option<ClickHandler>,
    on_done: Option<ClickHandler>,
    /// A done thread's: reopen it (the restore button on hover).
    on_reopen: Option<ClickHandler>,
    on_rename: Option<ClickHandler>,
    /// Pin or unpin it: whether it's pinned, whether it can be (it's saved), and the handler.
    on_pin: Option<(bool, bool, ClickHandler)>,
    /// While ⌘ is held: its number, and whether it still matches the digits typed so far.
    shortcut: Option<(SharedString, bool)>,
}

impl ThreadRow {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            selected: false,
            done: false,
            indicator: RowIndicator::None,
            force_hover: false,
            terminal: false,
            on_click: None,
            on_done: None,
            on_reopen: None,
            on_rename: None,
            on_pin: None,
            shortcut: None,
        }
    }

    /// Show `number` on the right (⌘ is held); `matches`: the digits typed so far lead to it.
    pub fn shortcut(mut self, shortcut: Option<(SharedString, bool)>) -> Self {
        self.shortcut = shortcut;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// A done thread's row, listed under Done (a click opens it to read; no menu).
    pub fn done(mut self, done: bool) -> Self {
        self.done = done;
        self
    }

    pub fn indicator(mut self, indicator: RowIndicator) -> Self {
        self.indicator = indicator;
        self
    }

    pub fn terminal(mut self, terminal: bool) -> Self {
        self.terminal = terminal;
        self
    }

    pub fn force_hover(mut self, hover: bool) -> Self {
        self.force_hover = hover;
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(std::rc::Rc::new(f));
        self
    }

    /// The row's context menu offers Rename Thread….
    pub fn on_rename(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_rename = Some(std::rc::Rc::new(f));
        self
    }

    /// The row's context menu offers Pin Thread (Unpin Thread when `pinned`), dimmed and inert
    /// unless `enabled` (a thread with nothing sent isn't saved, so it can't be pinned).
    pub fn on_pin(mut self, pinned: bool, enabled: bool, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_pin = Some((pinned, enabled, std::rc::Rc::new(f)));
        self
    }

    /// Mark it done: the archive button on hover (in place of the indicator), and Mark as Done
    /// in the context menu.
    pub fn on_done(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_done = Some(std::rc::Rc::new(f));
        self
    }

    /// A done row's: reopen it, from the restore button on hover (where an open row has its
    /// archive button).
    pub fn on_reopen(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_reopen = Some(std::rc::Rc::new(f));
        self
    }
}

impl RenderOnce for ThreadRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let menu_key = self.id.to_string();
        let indicator: Option<AnyElement> = match self.indicator {
            RowIndicator::None => None,
            RowIndicator::Working => Some(Dots::new().into_any_element()),
            RowIndicator::NeedsInput => Some(status_dot(color::yellow()).into_any_element()),
            RowIndicator::Paused => Some(
                gpui::component::Icon::new(gpui::assets::IconName::CirclePause)
                    .size(size::ICON)
                    .text_color(color::yellow())
                    .into_any_element(),
            ),
            RowIndicator::Unread => Some(status_dot(color::blue()).into_any_element()),
            RowIndicator::UnreadError => Some(status_dot(color::red()).into_any_element()),
        };
        let indicator = indicator.or_else(|| {
            self.terminal.then(|| {
                gpui::component::Icon::new(gpui::assets::IconName::SquareTerminal).size(size::ICON).text_color(color::text_3()).into_any_element()
            })
        });
        let hover_bg = color::surface();
        // Whether the pointer is on the row, kept by `on_hover` below, so the hover button is
        // chosen here, not shown by a hover style (a hover `display` can panic GPUI; see
        // `hover_styles_never_change_display`).
        let hovered = window.use_keyed_state(SharedString::from(format!("thread-row-hover-{menu_key}")), cx, |_, _| false);
        let show_button = (self.force_hover || *hovered.read(cx)) && self.shortcut.is_none();
        // On hover the archive button (a done row's: restore) takes the indicator's place (not
        // while ⌘ shows numbers).
        let done = self.on_done.clone().filter(|_| show_button).map(|f| {
            icon_button(SharedString::from(format!("{menu_key}-done")), gpui::assets::IconName::Archive, true, false)
                .size(px(20.))
                .tip_action("Mark as Done", &MarkDone)
                .on_click(move |e, w, cx| {
                    cx.stop_propagation();
                    f(e, w, cx)
                })
                .into_any_element()
        });
        let done = done.or_else(|| {
            self.on_reopen.clone().filter(|_| show_button).map(|f| {
                icon_button(SharedString::from(format!("{menu_key}-reopen")), gpui::assets::IconName::ArchiveRestore, true, false)
                    .size(px(20.))
                    .tip("Reopen Thread")
                    .on_click(move |e, w, cx| {
                        cx.stop_propagation();
                        f(e, w, cx)
                    })
                    .into_any_element()
            })
        });
        let indicator = indicator.filter(|_| done.is_none()).map(|i| div().flex_none().flex().items_center().child(i));
        let row = div()
            .id(self.id)
            .on_hover(move |on, _, cx| {
                hovered.update(cx, |h, cx| {
                    if *h != *on {
                        *h = *on;
                        cx.notify();
                    }
                })
            })
            .flex()
            .items_center()
            .gap_2()
            .h(size::THREAD_ROW_H)
            .px_2p5()
            .rounded(radius::MD)
            .cursor_pointer()
            .text_size(text::MD)
            .map(|d| {
                if self.selected {
                    d.bg(color::pressed()).text_color(color::text())
                } else if self.force_hover {
                    d.bg(hover_bg).text_color(color::text())
                } else {
                    d.text_color(if self.done { color::text_3() } else { color::text_2() })
                        // Quieter than the selected fill, so hover never reads as a second selection.
                        .hover(|s| s.bg(hover_bg).text_color(color::text()))
                        .active(|s| s.bg(color::hover()))
                }
            })
            .child(div().flex_1().min_w_0().truncate().child(self.title))
            .children(indicator)
            .children(done)
            .children(self.shortcut.map(|(n, matches)| shortcut_badge(n, matches)))
            .when_some(self.on_click, |d, f| d.on_click(move |e, w, cx| f(e, w, cx)));
        let items = row_menu(self.on_pin, self.on_rename, self.on_done);
        if items.is_empty() {
            return row.into_any_element();
        }
        context_menu(row, menu_key, items, window, cx).into_any_element()
    }
}

/// A quiet header over a section of the thread list ("Pinned", or one of Done's: "Today",
/// "October"), aligned with the row titles; `first`: no space above it. Not clickable.
pub fn section_header(label: impl Into<SharedString>, first: bool) -> Div {
    div()
        .when(!first, |d| d.pt(size::SIDEBAR_SECTION_GAP))
        .child(
            div()
                .h(size::SIDEBAR_SECTION_H)
                .px_2p5()
                .flex()
                .items_center()
                .text_size(text::XS)
                .text_color(color::text_3())
                .child(label.into()),
        )
}

/// The header of the Done list at the bottom of the thread list: "Done" (a step brighter than
/// the date headers inside it), how many, and a chevron that says whether it's expanded; a
/// click expands or collapses it. `first`: nothing
/// above it.
pub fn done_header(count: usize, open: bool, first: bool) -> Stateful<Div> {
    div().id("done-header").when(!first, |d| d.pt(size::SIDEBAR_SECTION_GAP)).child(
        div()
            .h(size::SIDEBAR_SECTION_H)
            .px_2p5()
            .flex()
            .items_center()
            .gap_1p5()
            .cursor_pointer()
            .text_size(text::XS)
            // A step brighter than the section headers under it: their parent, and a control.
            .text_color(color::text_2())
            .hover(|s| s.text_color(color::text()))
            .child("Done")
            .child(div().text_color(color::text_3()).child(count.to_string()))
            .child(div().flex_1())
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).size(size::ICON_SM).text_color(color::text_3())),
    )
}

/// A row's right-click menu: Pin (or Unpin) Thread, Rename Thread… and Mark as Done, with the
/// shortcuts of their actions.
fn row_menu(on_pin: Option<(bool, bool, ClickHandler)>, on_rename: Option<ClickHandler>, on_done: Option<ClickHandler>) -> Vec<ContextItem> {
    let pick = |f: ClickHandler| move |w: &mut Window, cx: &mut App| f(&ClickEvent::default(), w, cx);
    let mut items = Vec::new();
    if let Some((pinned, enabled, f)) = on_pin {
        let label = if pinned { "Unpin Thread" } else { "Pin Thread" };
        items.push(ContextItem::new(label, pick(f)).shortcut(&TogglePinThread).disabled(!enabled));
    }
    if let Some(f) = on_rename {
        items.push(ContextItem::new("Rename Thread…", pick(f)).shortcut(&RenameThread));
    }
    if let Some(f) = on_done {
        items.push(ContextItem::new("Mark as Done", pick(f)).shortcut(&MarkDone));
    }
    items
}

/// A row's number while ⌘ is held: a small key cap in monospace. Rows the typed digits no longer lead to fade.
fn shortcut_badge(number: SharedString, matches: bool) -> Div {
    div()
        .flex_none()
        .h(px(18.))
        .min_w(px(18.))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(radius::SM)
        .border_1()
        .border_color(color::border_strong())
        .bg(color::surface_2())
        // Monospace: every digit the same width, so 1 and 11 line up and never jiggle.
        .font_family(text::MONO_FONT)
        .text_size(text::XS)
        .text_color(color::text_2())
        .when(!matches, |d| d.opacity(0.35))
        .child(number)
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    let row = |name: &'static str, f: fn(ThreadRow) -> ThreadRow| {
        Example::new(name, move |_, _| {
            f(ThreadRow::new(SharedString::from(name), "Fix the failing smoke test in space-core")).into_any_element()
        })
        .width(size::SIDEBAR_W - px(20.))
    };
    vec![
        ComponentPreview::new(Area::Sidebar, "Thread row", "One line per thread with an activity indicator.")
            .stacked()
            .examples([
                row("idle", |r| r),
                row("selected", |r| r.selected(true)),
                row("hover", |r| r.force_hover(true).on_done(|_, _, _| {})),
                row("hover, working", |r| r.force_hover(true).indicator(RowIndicator::Working).on_done(|_, _, _| {})),
                row("working", |r| r.indicator(RowIndicator::Working)),
                row("working, selected", |r| r.selected(true).indicator(RowIndicator::Working)),
                row("unread", |r| r.indicator(RowIndicator::Unread)),
                row("paused (waiting for the guardrail)", |r| r.indicator(RowIndicator::Paused)),
                row("error", |r| r.indicator(RowIndicator::UnreadError)),
                row("waiting for input", |r| r.indicator(RowIndicator::NeedsInput)),
                row("terminal running", |r| r.terminal(true)),
                row("⌘ held", |r| r.shortcut(Some(("3".into(), true)))),
                row("⌘ held, selected and working", |r| r.selected(true).indicator(RowIndicator::Working).shortcut(Some(("12".into(), true)))),
                row("⌘ held, not what's typed", |r| r.shortcut(Some(("4".into(), false)))),
                row("done", |r| r.done(true)),
                row("done, hover", |r| r.done(true).force_hover(true).on_reopen(|_, _, _| {})),
                row("done, open to read", |r| r.done(true).selected(true)),
                Example::new("pinned, the rest by date, Done collapsed", |_, _| {
                    let row = |title: &'static str| ThreadRow::new(SharedString::from(format!("list-{title}")), title);
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(div().child(section_header("Pinned", true)).child(row("Release checklist")))
                        .child(row("Guardrail notes"))
                        .child(div().child(section_header("Today", false)).child(row("Fix the failing smoke test in space-core")))
                        .child(div().child(section_header("Yesterday", false)).child(row("Bump the gpui-kit version")))
                        .child(div().child(section_header("Previous 7 Days", false)).child(row("Explain how plugin discovery works")))
                        .child(done_header(14, false, false))
                        .into_any_element()
                })
                .width(size::SIDEBAR_W - px(20.)),
                Example::new("Done expanded", |_, _| {
                    let row = |title: &'static str| ThreadRow::new(SharedString::from(format!("done-{title}")), title).done(true).into_any_element();
                    let section = |label: &'static str, first: bool, titles: &[&'static str]| {
                        let mut rows = titles.iter().map(|t| row(t));
                        let head = rows.next().map(|r| div().child(section_header(label, first)).child(r).into_any_element());
                        head.into_iter().chain(rows).collect::<Vec<_>>()
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(ThreadRow::new("open-row", "Fix the failing smoke test in space-core"))
                        .child(done_header(6, true, false))
                        .children(section("Today", true, &["Add a context ring to the composer"]))
                        .children(section("Previous 7 Days", false, &["Terminal plugin with alacritty", "Greeting exchange"]))
                        .children(section("Previous 30 Days", false, &["Sketch the plugin manifest"]))
                        .children(section("September", false, &["Release 0.1.0"]))
                        .children(section("2025", false, &["First prototype"]))
                        .into_any_element()
                })
                .width(size::SIDEBAR_W - px(20.)),
                Example::new("right-click menu", |window, cx| {
                    let noop: ClickHandler = std::rc::Rc::new(|_, _, _| {});
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(ThreadRow::new("menu-row", "Fix the failing smoke test in space-core").force_hover(true))
                        .child(div().pl_8().child(super::menu::context_menu_preview(&row_menu(Some((false, true, noop.clone())), Some(noop.clone()), Some(noop)), Some(1), window, cx)))
                        .into_any_element()
                })
                .width(size::SIDEBAR_W - px(20.)),
            ]),
    ]
}

#[cfg(test)]
mod tests {
    use super::RowIndicator as R;

    #[test]
    fn indicator_priority() {
        assert_eq!(R::resolve(false, true, true, true, true), R::NeedsInput);
        assert_eq!(R::resolve(true, true, true, false, false), R::Working);
        assert_eq!(R::resolve(false, false, false, true, true), R::UnreadError);
        assert_eq!(R::resolve(false, false, false, true, false), R::Unread);
        assert_eq!(R::resolve(true, false, false, true, false), R::None);
    }
}
