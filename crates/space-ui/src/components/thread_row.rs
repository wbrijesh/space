//! Sidebar thread row: one line with the thread title and an activity indicator; right-click
//! for Delete Thread.

use gpui::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::loaders::Dots;
use super::widgets::status_dot;
use crate::ClickHandler;
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
    indicator: RowIndicator,
    /// Preview only: paint the hover state without a pointer.
    force_hover: bool,
    on_click: Option<ClickHandler>,
    on_delete: Option<ClickHandler>,
}

impl ThreadRow {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            selected: false,
            indicator: RowIndicator::None,
            force_hover: false,
            on_click: None,
            on_delete: None,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn indicator(mut self, indicator: RowIndicator) -> Self {
        self.indicator = indicator;
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

    /// The row's context menu offers Delete Thread.
    pub fn on_delete(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_delete = Some(std::rc::Rc::new(f));
        self
    }
}

impl RenderOnce for ThreadRow {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
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
        let hover_bg = color::surface();
        let row = div()
            .id(self.id)
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
                    d.text_color(color::text_2())
                        // Quieter than the selected fill, so hover never reads as a second selection.
                        .hover(|s| s.bg(hover_bg).text_color(color::text()))
                        .active(|s| s.bg(color::hover()))
                }
            })
            .child(div().flex_1().min_w_0().truncate().child(self.title))
            .children(indicator)
            .when_some(self.on_click, |d, f| d.on_click(move |e, w, cx| f(e, w, cx)));
        match self.on_delete {
            Some(f) => row
                .context_menu(move |menu, _, _| {
                    let f = f.clone();
                    menu.item(PopupMenuItem::new("Delete Thread").on_click(move |e, w, cx| f(e, w, cx)))
                })
                .into_any_element(),
            None => row.into_any_element(),
        }
    }
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
                row("hover", |r| r.force_hover(true)),
                row("working", |r| r.indicator(RowIndicator::Working)),
                row("working, selected", |r| r.selected(true).indicator(RowIndicator::Working)),
                row("unread", |r| r.indicator(RowIndicator::Unread)),
                row("paused (waiting for the guardrail)", |r| r.indicator(RowIndicator::Paused)),
                row("error", |r| r.indicator(RowIndicator::UnreadError)),
                row("waiting for input", |r| r.indicator(RowIndicator::NeedsInput)),
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
