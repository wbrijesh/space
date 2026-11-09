//! The composer's model and effort picker: one control showing both ("Opus 5.5 Medium"). A
//! click opens a menu above it with a Model row and an Effort row; each opens its list in the
//! same menu. Picking closes it; Esc steps back from a list, then closes.

use std::rc::Rc;

use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::protocol::{ConfigureSession, ModelInfo, SpaceStatus};

use super::menu::{chevron, choice, popover, row, row_with, surface, trigger};
use crate::format;
use crate::theme::{color, size, text};

/// What the open menu shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerPane {
    /// The Model and Effort rows.
    Root,
    Models,
    Efforts,
}

impl PickerPane {
    /// Esc from here: a list goes back to the rows, the rows close the menu.
    pub fn back(self) -> Option<PickerPane> {
        match self {
            PickerPane::Root => None,
            _ => Some(PickerPane::Root),
        }
    }
}

/// Open the menu at a pane, or close it (`None`).
pub type PaneHandler = Rc<dyn Fn(Option<PickerPane>, &mut Window, &mut App)>;
pub type ConfigureHandler = Rc<dyn Fn(ConfigureSession, &mut Window, &mut App)>;

/// The control (and its menu while `open`). `None` when the agent reports no model.
pub fn model_picker(
    status: &SpaceStatus,
    locked: bool,
    open: Option<PickerPane>,
    on_pane: PaneHandler,
    on_configure: ConfigureHandler,
) -> Option<Stateful<Div>> {
    let model = status.model.clone()?;
    let models = status.available_models.clone().unwrap_or_default();
    let levels = status.thinking_levels.clone().unwrap_or_default();
    let effort = status.thinking_level.clone().filter(|_| !levels.is_empty());
    let model_label = format::model_label(&model, status.current_model());
    let effort_label = effort.as_deref().map(format::effort_label);
    let open = open.filter(|_| !locked);
    let toggle = on_pane.clone();
    Some(
        trigger("model-picker", open.is_some(), !locked)
            .gap_1()
            .when(!locked, |d| {
                d.on_click(move |_, w, cx| toggle(if open.is_some() { None } else { Some(PickerPane::Root) }, w, cx))
            })
            .child(div().child(model_label.clone()))
            .children(effort_label.clone().map(|e| div().text_color(color::text_3()).child(e)))
            .child(chevron())
            .when_some(open, |d, pane| {
                let menu = match pane {
                    PickerPane::Root => root_pane(&model_label, effort_label.as_deref(), on_pane.clone()),
                    PickerPane::Models => models_pane(&models, &model, on_pane.clone(), on_configure),
                    PickerPane::Efforts => efforts_pane(&levels, effort.as_deref(), on_pane.clone(), on_configure),
                };
                let close = on_pane.clone();
                d.child(popover(true, surface().child(menu), move |w, cx| close(None, w, cx)))
            }),
    )
}

fn root_pane(model: &str, effort: Option<&str>, on_pane: PaneHandler) -> Div {
    let cell = |id: &'static str, label: &'static str, value: String, to: PickerPane| {
        let go = on_pane.clone();
        row(id)
            .child(div().flex_1().child(label))
            .child(div().text_color(color::text_2()).truncate().child(value))
            .child(Icon::new(IconName::ChevronRight).size(size::ICON).text_color(color::text_3()))
            .on_click(move |_, w, cx| go(Some(to), w, cx))
    };
    div()
        .flex()
        .flex_col()
        .child(cell("picker-model", "Model", model.to_string(), PickerPane::Models))
        .children(effort.map(|e| cell("picker-effort", "Effort", e.to_string(), PickerPane::Efforts)))
}

/// A list pane: a back row titled `title`, then the choices.
fn list(title: &'static str, on_pane: PaneHandler, items: Vec<Stateful<Div>>) -> Div {
    let back = on_pane;
    div()
        .flex()
        .flex_col()
        .child(
            row_with("picker-back", |s| s.bg(color::hover()).text_color(color::text_2()))
                .h(px(28.))
                .gap_1()
                .px_1p5()
                .text_size(text::SM)
                .text_color(color::text_3())
                .child(Icon::new(IconName::ChevronLeft).size(size::ICON))
                .child(title)
                .on_click(move |_, w, cx| back(Some(PickerPane::Root), w, cx)),
        )
        .children(items)
}

fn models_pane(models: &[ModelInfo], current: &str, on_pane: PaneHandler, on_configure: ConfigureHandler) -> Div {
    let items = models
        .iter()
        .map(|m| {
            let (provider, id, f, close) = (m.provider.clone(), m.id.clone(), on_configure.clone(), on_pane.clone());
            choice(ElementId::Name(format!("picker-model-{}", m.id).into()), None, format::model_label(&m.id, Some(m)), m.id == current).on_click(
                move |_, w, cx| {
                    close(None, w, cx);
                    f(ConfigureSession { provider: Some(provider.clone()), model: Some(id.clone()), ..Default::default() }, w, cx)
                },
            )
        })
        .collect();
    list("Model", on_pane, items)
}

fn efforts_pane(levels: &[String], current: Option<&str>, on_pane: PaneHandler, on_configure: ConfigureHandler) -> Div {
    let items = levels
        .iter()
        .map(|l| {
            let (level, f, close) = (l.clone(), on_configure.clone(), on_pane.clone());
            choice(ElementId::Name(format!("picker-effort-{l}").into()), None, format::effort_label(l), current == Some(l.as_str())).on_click(
                move |_, w, cx| {
                    close(None, w, cx);
                    f(ConfigureSession { thinking_level: Some(level.clone()), ..Default::default() }, w, cx)
                },
            )
        })
        .collect();
    list("Effort", on_pane, items)
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    let status = super::composer::fixtures::status(0);
    let pane = move |p: PickerPane| {
        let status = status.clone();
        move |_: &mut Window, _: &mut App| {
            let menu = match p {
                PickerPane::Root => root_pane("Opus 5.5", Some("Medium"), Rc::new(|_, _, _| {})),
                PickerPane::Models => models_pane(
                    status.available_models.as_deref().unwrap_or_default(),
                    "claude-opus-5-5",
                    Rc::new(|_, _, _| {}),
                    Rc::new(|_, _, _| {}),
                ),
                PickerPane::Efforts => efforts_pane(
                    status.thinking_levels.as_deref().unwrap_or_default(),
                    Some("medium"),
                    Rc::new(|_, _, _| {}),
                    Rc::new(|_, _, _| {}),
                ),
            };
            div().flex().child(surface().child(menu)).into_any_element()
        }
    };
    vec![ComponentPreview::new(Area::Composer, "Model picker", "Model and effort in one menu, rendered open.").examples([
        Example::new("menu", pane(PickerPane::Root)),
        Example::new("models", pane(PickerPane::Models)),
        Example::new("efforts", pane(PickerPane::Efforts)),
    ])]
}
