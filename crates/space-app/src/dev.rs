//! Dev tooling wired into the app (feature `dev`): the gallery window, animation-clock
//! shortcuts, and the Dev menu (with scenario switching).
//!
//! | Shortcut | |
//! |---|---|
//! | ⌥⌘G | open the component gallery |
//! | ⌥⌘P | pause / resume all animations |
//! | ⌥⌘. | step one frame (pauses) |
//! | ⌥⌘1 / ⌥⌘2 / ⌥⌘3 | animation speed 0.1× / 0.25× / 1× |
//! | ⌥⌘0 | unpin and resume at 1× |

use gpui::*;
use space_ui::actions::dev::*;
use space_ui::clock::{AnimationClock, STEP};

use crate::scenarios;

/// Relaunch the app in a scenario (Dev ▸ Scenarios).
#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
#[action(namespace = space_dev)]
pub struct OpenScenario {
    pub name: String,
}

pub fn init(cx: &mut App) {
    // `SPACE_CLOCK=paused | pin:0.35 | speed:0.25` starts the animation clock in that state.
    if let Ok(spec) = std::env::var("SPACE_CLOCK") {
        let mut result = Ok(());
        AnimationClock::update(cx, |c| result = c.apply_spec(&spec));
        if let Err(e) = result {
            eprintln!("SPACE_CLOCK: {e}");
        }
    }
    cx.bind_keys([
        KeyBinding::new("alt-cmd-g", OpenGallery, None),
        KeyBinding::new("alt-cmd-p", ToggleAnimations, None),
        KeyBinding::new("alt-cmd-.", StepAnimations, None),
        KeyBinding::new("alt-cmd-1", AnimationSpeedTenth, None),
        KeyBinding::new("alt-cmd-2", AnimationSpeedQuarter, None),
        KeyBinding::new("alt-cmd-3", AnimationSpeedNormal, None),
        KeyBinding::new("alt-cmd-0", UnpinAnimations, None),
    ]);
    cx.on_action(|_: &OpenGallery, cx| {
        space_ui::gallery::open(cx);
    });
    cx.on_action(|_: &ToggleAnimations, cx| AnimationClock::update(cx, |c| c.toggle_pause()));
    cx.on_action(|_: &StepAnimations, cx| {
        AnimationClock::update(cx, |c| {
            c.pause();
            c.step(STEP);
        })
    });
    cx.on_action(|_: &AnimationSpeedTenth, cx| AnimationClock::update(cx, |c| c.set_speed(0.1)));
    cx.on_action(|_: &AnimationSpeedQuarter, cx| AnimationClock::update(cx, |c| c.set_speed(0.25)));
    cx.on_action(|_: &AnimationSpeedNormal, cx| AnimationClock::update(cx, |c| c.set_speed(1.)));
    cx.on_action(|_: &UnpinAnimations, cx| AnimationClock::update(cx, |c| c.reset()));
    cx.on_action(|a: &OpenScenario, cx| {
        let relaunched = std::env::current_exe().and_then(|exe| {
            std::process::Command::new(exe).arg("--scenario").arg(&a.name).arg(std::env::current_dir()?).spawn()
        });
        match relaunched {
            Ok(_) => cx.quit(),
            Err(e) => eprintln!("could not relaunch in scenario {}: {e}", a.name),
        }
    });
}

pub fn menu() -> Menu {
    let scenarios = scenarios::all()
        .into_iter()
        .map(|s| MenuItem::action(s.name, OpenScenario { name: s.name.to_string() }))
        .collect();
    Menu {
        name: "Dev".into(),
        items: vec![
            MenuItem::action("Component Gallery", OpenGallery),
            MenuItem::separator(),
            MenuItem::action("Pause / Resume Animations", ToggleAnimations),
            MenuItem::action("Step Animation Frame", StepAnimations),
            MenuItem::action("Animation Speed 0.1×", AnimationSpeedTenth),
            MenuItem::action("Animation Speed 0.25×", AnimationSpeedQuarter),
            MenuItem::action("Animation Speed 1×", AnimationSpeedNormal),
            MenuItem::action("Reset Animation Clock", UnpinAnimations),
            MenuItem::separator(),
            MenuItem::submenu(Menu { name: "Relaunch in Scenario".into(), items: scenarios, disabled: false }),
        ],
        disabled: false,
    }
}
