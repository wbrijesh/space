//! The server's menu bar item: what the server is doing, Open Space, Quit Server…. It runs on the
//! process's main thread (AppKit's), redrawn each second from a [`Menu`] the server keeps current;
//! choices go back to the server as [`Action`]s.

use std::sync::{Arc, Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSAlert, NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
use objc2_foundation::{NSString, NSTimer};
use tokio::sync::mpsc;

/// What the menu shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Menu {
    /// Lines about the server (not clickable).
    pub lines: Vec<String>,
    /// What can be done about an update, under the lines.
    pub update: Vec<(String, Action)>,
    /// What Quit Server would stop, for its confirmation ("2 threads working and 3 terminals").
    pub stops: Option<String>,
}

/// A choice in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    OpenSpace,
    /// Switch to the ready update once no thread is working.
    UpdateWhenIdle,
    /// Switch now.
    UpdateNow,
    QuitServer,
}

static ACTIONS: OnceLock<mpsc::UnboundedSender<Action>> = OnceLock::new();
static MODEL: OnceLock<Arc<Mutex<Menu>>> = OnceLock::new();

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpaceServerMenuTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(openSpace:))]
        fn open_space(&self, _sender: &AnyObject) {
            send(Action::OpenSpace);
        }

        #[unsafe(method(updateWhenIdle:))]
        fn update_when_idle(&self, _sender: &AnyObject) {
            send(Action::UpdateWhenIdle);
        }

        #[unsafe(method(updateNow:))]
        fn update_now(&self, _sender: &AnyObject) {
            send(Action::UpdateNow);
        }

        #[unsafe(method(quitServer:))]
        fn quit_server(&self, _sender: &AnyObject) {
            let stops = MODEL.get().and_then(|m| m.lock().unwrap().stops.clone());
            if confirm_quit(stops.as_deref()) {
                send(Action::QuitServer);
            }
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

fn send(action: Action) {
    if let Some(tx) = ACTIONS.get() {
        let _ = tx.send(action);
    }
}

/// Quit Server…: ask when something would stop.
fn confirm_quit(stops: Option<&str>) -> bool {
    let Some(stops) = stops else { return true };
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let app = NSApplication::sharedApplication(mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("Quit the Space server?"));
    alert.setInformativeText(&NSString::from_str(&format!("{stops} will stop.")));
    alert.addButtonWithTitle(&NSString::from_str("Quit Server"));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    // The first button.
    alert.runModal() == 1000
}

/// Show the menu bar item and run AppKit's loop on this (the main) thread. Never returns.
pub fn run(model: Arc<Mutex<Menu>>, actions: mpsc::UnboundedSender<Action>) -> ! {
    let mtm = MainThreadMarker::new().expect("the menu bar runs on the main thread");
    let _ = ACTIONS.set(actions);
    let _ = MODEL.set(model.clone());
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let item: Retained<NSStatusItem> = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        match NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str("square.stack"), Some(&NSString::from_str("Space server"))) {
            Some(image) => {
                image.setTemplate(true);
                button.setImage(Some(&image));
            }
            None => button.setTitle(&NSString::from_str("Space")),
        }
    }
    let target = Target::new(mtm);
    let menu = NSMenu::new(mtm);
    item.setMenu(Some(&menu));
    let shown: Mutex<Option<Menu>> = Mutex::new(None);
    let refresh = {
        let (menu, target) = (menu.clone(), target.clone());
        move || {
            let now = model.lock().unwrap().clone();
            let mut last = shown.lock().unwrap();
            if last.as_ref() == Some(&now) {
                return;
            }
            fill(&menu, &target, &now, mtm);
            *last = Some(now);
        }
    };
    refresh();
    let block = block2::RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| refresh());
    let _timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, true, &block) };
    app.run();
    std::process::exit(0)
}

fn fill(menu: &NSMenu, target: &Target, model: &Menu, mtm: MainThreadMarker) {
    menu.removeAllItems();
    let add = |title: &str, action: Option<objc2::runtime::Sel>| {
        let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), action, &NSString::from_str("")) };
        match action {
            Some(_) => unsafe { item.setTarget(Some(target)) },
            None => item.setEnabled(false),
        }
        menu.addItem(&item);
    };
    for line in &model.lines {
        add(line, None);
    }
    for (title, action) in &model.update {
        let selector = match action {
            Action::UpdateWhenIdle => sel!(updateWhenIdle:),
            Action::UpdateNow => sel!(updateNow:),
            Action::OpenSpace => sel!(openSpace:),
            Action::QuitServer => sel!(quitServer:),
        };
        add(title, Some(selector));
    }
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    add("Open Space", Some(sel!(openSpace:)));
    add("Quit Server…", Some(sel!(quitServer:)));
}
