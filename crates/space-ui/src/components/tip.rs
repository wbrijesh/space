//! Hover labels: a short label (and the key binding, when it has one) placed under the element
//! it describes, centred on it, above it when there's no room below. Not where the pointer is.
//!
//! `.tip("Copy")` / `.tip_action("Back", &GoBack)` on any element with an id; each window's
//! root renders [`layer`] once, where the label is drawn. Moving between labelled elements
//! switches labels without waiting again; a click hides the label until the pointer leaves.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use gpui::base::{Placement, TooltipOverlay, TooltipRequest, TooltipTransition};
use gpui::component::kbd::Kbd;
use gpui::*;

use crate::theme::{color, motion, radius, text};

/// Gap between the element and its label.
const GAP: Pixels = px(6.);

type Build = Rc<dyn Fn(&mut Window, &mut App) -> AnyView>;

pub trait TipExt: StatefulInteractiveElement + ParentElement + Sized {
    /// A plain label.
    fn tip(self, label: impl Into<SharedString>) -> Self {
        let label: SharedString = label.into();
        with_tip(self, Rc::new(move |_, cx| cx.new(|_| Tip { label: label.clone(), keys: None }).into()))
    }

    /// A label with `action`'s key binding beside it.
    fn tip_action(self, label: impl Into<SharedString>, action: &dyn Action) -> Self {
        let label: SharedString = label.into();
        let action = action.boxed_clone();
        with_tip(
            self,
            Rc::new(move |window, cx| {
                let keys = window
                    .highest_precedence_binding_for_action(action.as_ref())
                    .and_then(|b| b.keystrokes().first().map(|k| Kbd::format(k.as_keystroke()).into()));
                cx.new(|_| Tip { label: label.clone(), keys }).into()
            }),
        )
    }
}

impl<E: StatefulInteractiveElement + ParentElement> TipExt for E {}

/// Where each window draws its labels, and which element the pointer is on.
#[derive(Default)]
struct Tips {
    overlays: HashMap<WindowId, Entity<TooltipOverlay>>,
    /// The element whose label is shown or pending. It drops out when the element isn't drawn.
    hovered: Option<Weak<()>>,
    /// Clicked: its label stays hidden until the pointer leaves it.
    pressed: Option<Weak<()>>,
}

impl Global for Tips {}

/// Draws this window's labels; render it once, anywhere in the window's root.
pub fn layer(window: &mut Window, cx: &mut App) -> AnyElement {
    let id = window.window_handle().window_id();
    let tips = cx.default_global::<Tips>();
    let overlay = match tips.overlays.get(&id) {
        Some(o) => o.clone(),
        None => {
            let o = cx.new(|_| TooltipOverlay::new().render_with(appear));
            cx.global_mut::<Tips>().overlays.insert(id, o.clone());
            o
        }
    };
    // An element can vanish without the pointer leaving it (a click that changes the view):
    // once this frame is drawn, drop its label.
    if cx.global::<Tips>().hovered.is_some() {
        let o = overlay.clone();
        window.defer(cx, move |_, cx| {
            let gone = cx.global::<Tips>().hovered.as_ref().is_some_and(|h| h.upgrade().is_none());
            if gone {
                cx.global_mut::<Tips>().hovered = None;
                o.update(cx, |o, cx| o.hide(cx));
            }
        });
    }
    overlay.into_any_element()
}

fn overlay(window: &Window, cx: &App) -> Option<Entity<TooltipOverlay>> {
    cx.try_global::<Tips>()?.overlays.get(&window.window_handle().window_id()).cloned()
}

fn same(a: &Option<Weak<()>>, b: &Weak<()>) -> bool {
    a.as_ref().is_some_and(|a| a.ptr_eq(b))
}

#[derive(Default)]
struct Trigger {
    bounds: Bounds<Pixels>,
    /// Lives while the element is drawn (kept in its element state).
    alive: Weak<()>,
}

fn with_tip<E: StatefulInteractiveElement + ParentElement>(el: E, build: Build) -> E {
    let trigger = Rc::new(RefCell::new(Trigger::default()));
    let (writer, hover, press) = (trigger.clone(), trigger.clone(), trigger);
    el.child(
        canvas(
            move |bounds, window, _| {
                let alive = window.with_global_id("space-tip".into(), |id, window| {
                    window.with_element_state(id, |s: Option<Rc<()>>, _| {
                        let s = s.unwrap_or_default();
                        (Rc::downgrade(&s), s)
                    })
                });
                *writer.borrow_mut() = Trigger { bounds, alive };
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full(),
    )
    .on_hover(move |hovered, window, cx| {
        let (bounds, alive) = {
            let t = hover.borrow();
            (t.bounds, t.alive.clone())
        };
        let Some(overlay) = overlay(window, cx) else { return };
        let tips = cx.global_mut::<Tips>();
        if *hovered {
            if same(&tips.pressed, &alive) {
                return;
            }
            tips.hovered = Some(alive);
            let build = build.clone();
            let request = TooltipRequest::new(bounds, move |w, cx| build(w, cx)).with_placement(Placement::Bottom);
            overlay.update(cx, |o, cx| o.request_show(request, window, cx));
        } else {
            if same(&tips.pressed, &alive) {
                tips.pressed = None;
            }
            // Hover events run in reverse paint order, so the next element may already have
            // taken over; only the current one hides.
            if same(&tips.hovered, &alive) {
                tips.hovered = None;
                overlay.update(cx, |o, cx| o.request_hide(window, cx));
            }
        }
    })
    // Capture phase: buttons stop the click's propagation in the bubble phase.
    .capture_any_mouse_down(move |_, window, cx| {
        let alive = press.borrow().alive.clone();
        let Some(overlay) = overlay(window, cx) else { return };
        let tips = cx.global_mut::<Tips>();
        tips.pressed = Some(alive);
        tips.hovered = None;
        overlay.update(cx, |o, cx| o.hide(cx));
    })
}

/// A fresh label fades in; switching to the next element's label moves it straight there.
fn appear(view: AnyView, transition: TooltipTransition, _: &mut Window, _: &mut App) -> AnyElement {
    let el = div().p(GAP).child(view);
    match transition {
        TooltipTransition::Enter { epoch } => el
            .with_animation(
                ElementId::NamedInteger("space-tip-in".into(), epoch as u64),
                Animation::new(motion::TIP_FADE).with_easing(motion::ease_out),
                |el, t| el.opacity(t),
            )
            .into_any_element(),
        TooltipTransition::Switch { .. } => el.into_any_element(),
    }
}

struct Tip {
    label: SharedString,
    keys: Option<SharedString>,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        label(self.label.clone(), self.keys.clone())
    }
}

/// The label itself.
pub fn label(text: SharedString, keys: Option<SharedString>) -> Div {
    div()
        .h(px(26.))
        .flex()
        .items_center()
        .gap_2()
        .pl_2()
        .pr(if keys.is_some() { px(4.) } else { px(8.) })
        .rounded(radius::LG)
        .bg(color::surface_2())
        .border_1()
        .border_color(color::border_strong())
        .shadow_md()
        .font_family(text::UI_FONT)
        .text_size(text::SM)
        .text_color(color::text())
        .whitespace_nowrap()
        .child(text)
        .children(keys.map(|k| {
            div()
                .h(px(18.))
                .px_1()
                .flex()
                .items_center()
                .rounded(radius::XS)
                .border_1()
                .border_color(color::border_strong())
                .text_size(text::XS)
                .text_color(color::text_2())
                .child(k)
        }))
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    vec![ComponentPreview::new(Area::Shell, "Hover label", "Under the element it describes, centred on it.").examples([
        Example::new("plain", |_, _| label("Copy".into(), None).into_any_element()),
        Example::new("with key", |_, _| label("Back".into(), Some("⌘[".into())).into_any_element()),
    ])]
}
