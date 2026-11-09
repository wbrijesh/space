//! The row under a message, shown while the pointer is over the message: copy, the turn's usage
//! (agent replies) and how long ago it was. Its height is always kept, so showing it doesn't
//! move anything.

use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::Icon;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::protocol::TurnUsage;

use super::tip::TipExt as _;
use crate::format;
use crate::theme::{color, radius, size, text};

/// The row's height (kept while it's hidden).
pub const ROW_H: Pixels = px(28.);

/// Copy `text` (the row's id says which row shows "Copied").
pub type CopyHandler = Rc<dyn Fn(SharedString, SharedString, &mut Window, &mut App)>;
/// Open (`Some(row id)`) or close (`None`) a row's usage panel.
pub type UsageHandler = Rc<dyn Fn(Option<SharedString>, &mut Window, &mut App)>;

#[derive(Clone)]
pub struct ActionHandlers {
    pub on_copy: CopyHandler,
    pub on_usage: UsageHandler,
}

impl Default for ActionHandlers {
    fn default() -> Self {
        Self { on_copy: Rc::new(|_, _, _, _| {}), on_usage: Rc::new(|_, _, _| {}) }
    }
}

/// One message's row.
pub struct MessageActions {
    /// Unique in the window.
    pub id: SharedString,
    /// The hover group of the message it belongs to: hovering it shows the row.
    pub group: SharedString,
    /// What copy copies; no copy button without it.
    pub copy_text: Option<SharedString>,
    /// Copy was just clicked: a check and "Copied".
    pub copied: bool,
    pub at_ms: Option<u64>,
    pub now_ms: u64,
    pub usage: Option<TurnUsage>,
    /// The usage panel is open (the row stays shown).
    pub usage_open: bool,
    /// Right-aligned, time first (your messages); else left-aligned, copy first.
    pub end: bool,
    /// Shown without hovering (previews).
    pub shown: bool,
}

pub fn message_actions(a: MessageActions, h: &ActionHandlers) -> Div {
    let MessageActions { id, group, copy_text, copied, at_ms, now_ms, usage, usage_open, end, shown } = a;
    let copy = copy_text.map(|copy_text| {
        let (on_copy, row) = (h.on_copy.clone(), id.clone());
        div()
            .id(ElementId::Name(format!("{id}-copy").into()))
            .h(px(24.))
            .px_1p5()
            .flex()
            .items_center()
            .gap_1()
            .rounded(radius::MD)
            .text_color(if copied { color::text() } else { color::text_3() })
            .cursor_pointer()
            .hover(|s| s.bg(color::hover()).text_color(color::text()))
            .child(Icon::new(if copied { Lucide::Check } else { Lucide::Copy }).size(size::ICON))
            .when(copied, |d| d.child("Copied"))
            .when(!copied, |d| d.tip("Copy"))
            .on_click(move |_, w, cx| on_copy(row.clone(), copy_text.clone(), w, cx))
            .into_any_element()
    });
    let usage = usage.map(|u| usage_pill(&id, u, usage_open, h.on_usage.clone()).into_any_element());
    let time = at_ms.map(|at| {
        div()
            .id(ElementId::Name(format!("{id}-time").into()))
            .px_1()
            .child(format::time_ago(at, now_ms))
            .tip(format::exact_time(at))
            .into_any_element()
    });
    let items: Vec<AnyElement> = if end { [time, copy].into_iter().flatten().collect() } else { [copy, usage, time].into_iter().flatten().collect() };
    div()
        .h(ROW_H)
        .flex()
        .items_center()
        .gap_1()
        .when(end, |d| d.justify_end())
        .text_size(text::SM)
        .text_color(color::text_3())
        .when(!usage_open && !shown, |d| d.invisible().group_hover(group, |s| s.visible()))
        .children(items)
}

/// "Usage 9.4k tok"; a click opens the turn's usage above it.
fn usage_pill(id: &SharedString, usage: TurnUsage, open: bool, on_usage: UsageHandler) -> Stateful<Div> {
    let row = id.clone();
    let toggle = on_usage.clone();
    div()
        .id(ElementId::Name(format!("{id}-usage").into()))
        .relative()
        .h(px(24.))
        .px_1p5()
        .flex()
        .items_center()
        .gap_1p5()
        .rounded(radius::MD)
        .cursor_pointer()
        .when(open, |d| d.bg(color::hover()).text_color(color::text()))
        .hover(|s| s.bg(color::hover()).text_color(color::text()))
        .child(Icon::new(Lucide::Database).size(size::ICON))
        .child(format!("Usage {} tok", format::compact_tokens(usage.total())))
        .on_click(move |_, w, cx| toggle(if open { None } else { Some(row.clone()) }, w, cx))
        .when(open, |d| {
            // Above the pill, kept inside the window.
            d.child(
                div().absolute().top_0().left_0().child(deferred(
                    anchored().anchor(Anchor::BottomLeft).snap_to_window_with_margin(px(8.)).child(
                        div().pb_1().child(usage_panel(&usage).on_mouse_down_out(move |_, w, cx| on_usage(None, w, cx))),
                    ),
                )),
            )
        })
}

/// The turn's usage in full: total, model, cache hit and each bucket.
pub fn usage_panel(u: &TurnUsage) -> Div {
    let tok = |n: u64| format!("{} tok", format::thousands(n));
    let row = |label: &str, value: String| {
        div()
            .flex()
            .justify_between()
            .gap_6()
            .child(div().text_color(color::text_3()).child(label.to_string()))
            .child(div().text_color(color::text()).child(value))
    };
    let mut rows = Vec::new();
    if !u.models.is_empty() {
        rows.push(row(if u.models.len() == 1 { "Model" } else { "Models" }, u.models.join(", ")));
    }
    if let Some(hit) = u.cache_hit_percent() {
        rows.push(row("Cache hit", format!("{hit}%")));
    }
    rows.push(row("Uncached input", tok(u.input)));
    if let Some(read) = u.cache_read {
        rows.push(row("Cached input", tok(read)));
    }
    if let Some(write) = u.cache_write {
        rows.push(row("Cache write", tok(write)));
    }
    rows.push(row("Output", tok(u.output)));
    if let Some(r) = u.reasoning.filter(|r| *r > 0) {
        rows.push(row("  of which reasoning", tok(r)));
    }
    div()
        .min_w(px(300.))
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(radius::XL)
        .bg(color::surface())
        .border_1()
        .border_color(color::border())
        .shadow_lg()
        .text_size(text::SM)
        .cursor_default()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_6()
                .pb_2()
                .border_b_1()
                .border_color(color::border())
                .font_weight(FontWeight::MEDIUM)
                .child(div().flex().items_center().gap_2().child(Icon::new(Lucide::Database).size(size::ICON)).child("Turn usage"))
                .child(tok(u.total())),
        )
        .children(rows)
}

#[cfg(feature = "dev")]
pub fn sample_usage() -> TurnUsage {
    TurnUsage {
        input: 1204,
        output: 97,
        cache_read: Some(8112),
        cache_write: Some(88),
        reasoning: Some(41),
        models: vec!["claude-cli/claude-opus-5-5".into()],
    }
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    let now = 1_000_000_000_000;
    let row = move |end: bool, copied: bool, usage: Option<TurnUsage>, ago_s: u64| {
        let a = MessageActions {
            id: format!("preview-row-{end}-{copied}-{ago_s}").into(),
            group: "never-hovered".into(),
            copy_text: Some("text".into()),
            copied,
            at_ms: Some(now - ago_s * 1000),
            now_ms: now,
            usage,
            usage_open: false,
            end,
            shown: true,
        };
        message_actions(a, &ActionHandlers::default())
    };
    vec![ComponentPreview::new(Area::Transcript, "Message actions", "Shown while the pointer is over the message.")
        .stacked()
        .examples([
            Example::new("your message", move |_, _| row(true, false, None, 180).into_any_element()),
            Example::new("agent reply", move |_, _| row(false, false, Some(sample_usage()), 120).into_any_element()),
            Example::new("copied", move |_, _| row(false, true, Some(sample_usage()), 7 * 3600).into_any_element()),
            Example::new("usage panel", |_, _| div().flex().child(usage_panel(&sample_usage())).into_any_element()),
        ])]
}
