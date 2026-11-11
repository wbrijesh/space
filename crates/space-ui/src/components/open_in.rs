//! "Open in": the title bar's button that opens the thread's folder in an app (the one used
//! last), and its menu of the installed apps.

use std::rc::Rc;

use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::menu;
use super::tip::TipExt as _;
use crate::actions::OpenInLastApp;
use crate::theme::{color, radius, size, text};

/// An app the folder can open in.
#[derive(Clone)]
pub struct OpenInApp {
    /// Its bundle id.
    pub id: SharedString,
    pub name: SharedString,
    /// Its icon (`None`: not drawn yet, or none).
    pub icon: Option<ImageSource>,
    /// Editors, then Finder, then terminals: a separator goes between groups.
    pub group: u8,
}

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
pub type PickHandler = Rc<dyn Fn(SharedString, &mut Window, &mut App)>;
pub type BoolHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// What the control shows and does.
#[derive(Clone)]
pub struct OpenIn {
    /// The app a click opens in (the one used last).
    pub current: OpenInApp,
    /// The installed apps, in menu order.
    pub apps: Vec<OpenInApp>,
    pub menu_open: bool,
    pub on_open: Handler,
    /// Open (`true`) or close the menu.
    pub on_menu: BoolHandler,
    pub on_pick: PickHandler,
    pub on_copy_path: Handler,
}

/// An app's icon at `side`, or a quiet placeholder.
pub fn app_icon(icon: Option<ImageSource>, side: Pixels) -> AnyElement {
    match icon {
        Some(src) => img(src).size(side).flex_none().into_any_element(),
        None => div().size(side).flex_none().rounded(radius::XS).bg(color::pressed()).into_any_element(),
    }
}

/// The control: the current app's icon (opens the folder there) and a chevron (the menu).
pub fn open_in_control(o: OpenIn) -> Div {
    let OpenIn { current, apps, menu_open, on_open, on_menu, on_pick, on_copy_path } = o;
    let toggle = on_menu.clone();
    let close = on_menu.clone();
    let open = div()
        .id("open-in")
        .h(px(26.))
        .pl(px(6.))
        .pr_1()
        .flex()
        .items_center()
        .rounded_l(radius::MD)
        .cursor_pointer()
        .hover(|s| s.bg(color::hover()))
        .child(app_icon(current.icon.clone(), px(16.)))
        .tip_action(format!("Open in {}", current.name), &OpenInLastApp)
        .on_click(move |_, w, cx| on_open(w, cx));
    let chevron = div()
        .id("open-in-menu")
        .relative()
        .h(px(26.))
        .px_1()
        .flex()
        .items_center()
        .rounded_r(radius::MD)
        .cursor_pointer()
        .hover(|s| s.bg(color::hover()))
        .when(menu_open, |d| d.bg(color::hover()))
        .child(menu::chevron())
        .on_click(move |_, w, cx| toggle(!menu_open, w, cx))
        .when(menu_open, |d| {
            d.child(menu::dropdown(true, open_in_menu(&apps, on_pick, on_copy_path), move |w, cx| close(false, w, cx)))
        });
    div().flex().items_center().child(open).child(chevron)
}

/// The menu: each app with its icon and name (editors, Finder, terminals), then Copy Path.
pub fn open_in_menu(apps: &[OpenInApp], on_pick: PickHandler, on_copy_path: Handler) -> Div {
    let mut card = menu::surface().min_w(px(200.)).max_w(px(260.));
    let mut last_group = None;
    for (ix, app) in apps.iter().enumerate() {
        if last_group.is_some_and(|g| g != app.group) {
            card = card.child(menu::separator());
        }
        last_group = Some(app.group);
        let (pick, id) = (on_pick.clone(), app.id.clone());
        card = card.child(
            menu::row(("open-in-app", ix))
                .gap_2()
                .child(app_icon(app.icon.clone(), px(16.)))
                .child(div().flex_1().min_w_0().truncate().child(app.name.clone()))
                .on_click(move |_, w, cx| pick(id.clone(), w, cx)),
        );
    }
    if !apps.is_empty() {
        card = card.child(menu::separator());
    }
    card.child(
        menu::row("open-in-copy-path")
            .gap_2()
            .child(div().size(px(16.)).flex().items_center().justify_center().text_color(color::text_2()).child(Icon::new(IconName::Copy).size(size::ICON_SM)))
            .child(div().flex_1().child("Copy Path"))
            .on_click(move |_, w, cx| on_copy_path(w, cx)),
    )
    .text_size(text::MD)
}

/// Fixture apps for previews (no real icons).
#[cfg(feature = "dev")]
pub fn fixture() -> OpenIn {
    let app = |id: &'static str, name: &'static str, group| OpenInApp { id: id.into(), name: name.into(), icon: None, group };
    let apps = vec![
        app("dev.zed.Zed", "Zed", 0),
        app("com.microsoft.VSCode", "VS Code", 0),
        app("com.apple.dt.Xcode", "Xcode", 0),
        app("com.apple.finder", "Finder", 1),
        app("com.mitchellh.ghostty", "Ghostty", 2),
        app("com.apple.Terminal", "Terminal", 2),
    ];
    OpenIn {
        current: apps[0].clone(),
        apps,
        menu_open: false,
        on_open: Rc::new(|_, _| {}),
        on_menu: Rc::new(|_, _, _| {}),
        on_pick: Rc::new(|_, _, _| {}),
        on_copy_path: Rc::new(|_, _| {}),
    }
}
