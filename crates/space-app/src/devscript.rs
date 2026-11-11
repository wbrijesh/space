//! Dev-only input scripting: `SPACE_SCRIPT=path` replays a list of input commands through
//! GPUI's own event dispatch (`Window::dispatch_event`), so hit testing, focus transfer,
//! hover and active states run exactly as for real mouse input. Used to verify UI behavior
//! (and take screenshots) without OS-level event synthesis.
//!
//! One command per line (`#` starts a comment):
//!
//! ```text
//! wait 500            sleep 500 ms
//! move 120 300        move the mouse to window coordinates (logical px)
//! click 120 300       move + left mouse down + up
//! rclick 120 300      the same with the right button (context menus)
//! scroll 400 300 -120 trackpad scroll at (400, 300) moving the view 120 px (negative: up)
//! key cmd-n           dispatch a keystroke
//! type some text      type text into the focused element
//! resize 1280 820     resize the window (logical px)
//! size                print the window's size and scale factor
//! context             print the focused element's key context stack
//! composer            print the composer's text
//! find                print find's state (query, matches, current) and the transcript's scroll
//! rename some name    rename the selected thread as the Rename sheet would (then print its title)
//! title               print the selected thread's title
//! editmenu            print the Edit menu's items: AppKit action and key equivalent
//! terminals           print the selected thread's terminal pane: open, its tabs, how far it's shown
//! shot name           run `$SPACE_SHOT_CMD name` (e.g. a script calling `screencapture -l`)
//! shotnow name        same as `shot` without the 250 ms settle delay (animation frames)
//! quit                quit the app
//! ```

use std::time::Duration;

use gpui::*;

/// A script is running (enables extra diagnostics on stderr).
pub fn active() -> bool {
    std::env::var_os("SPACE_SCRIPT").is_some()
}

pub fn spawn_from_env(window: AnyWindowHandle, cx: &mut App) {
    let Some(path) = std::env::var_os("SPACE_SCRIPT") else { return };
    let script = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("SPACE_SCRIPT: cannot read {}: {e}", path.to_string_lossy());
            return;
        }
    };
    cx.spawn(async move |cx| {
        let lines: Vec<String> = script.lines().map(str::to_string).collect();
        for raw in &lines {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let cmd = parts.next().unwrap_or("");
            let args: Vec<&str> = parts.collect();
            let num = |i: usize| args.get(i).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.);
            eprintln!("script: {line}");
            match cmd {
                "wait" => cx.background_executor().timer(Duration::from_millis(num(0) as u64)).await,
                "move" | "click" | "rclick" => {
                    let position = point(px(num(0)), px(num(1)));
                    let click = cmd != "move";
                    let button = if cmd == "rclick" { MouseButton::Right } else { MouseButton::Left };
                    let _ = window.update(cx, |_, window, cx| {
                        window.dispatch_event(
                            PlatformInput::MouseMove(MouseMoveEvent {
                                position,
                                pressed_button: None,
                                modifiers: Modifiers::default(),
                            }),
                            cx,
                        );
                        if click {
                            window.dispatch_event(
                                PlatformInput::MouseDown(MouseDownEvent {
                                    button,
                                    position,
                                    modifiers: Modifiers::default(),
                                    click_count: 1,
                                    first_mouse: false,
                                }),
                                cx,
                            );
                            window.dispatch_event(
                                PlatformInput::MouseUp(MouseUpEvent {
                                    button,
                                    position,
                                    modifiers: Modifiers::default(),
                                    click_count: 1,
                                }),
                                cx,
                            );
                        }
                        window.refresh();
                    });
                    cx.background_executor().timer(Duration::from_millis(16)).await;
                }
                "scroll" => {
                    let position = point(px(num(0)), px(num(1)));
                    // GPUI adds the delta to the offset, so content moving down = view moving up.
                    let delta = ScrollDelta::Pixels(point(px(0.), px(-num(2))));
                    let _ = window.update(cx, |_, window, cx| {
                        window.dispatch_event(
                            PlatformInput::MouseMove(MouseMoveEvent {
                                position,
                                pressed_button: None,
                                modifiers: Modifiers::default(),
                            }),
                            cx,
                        );
                        window.dispatch_event(
                            PlatformInput::Scroll(ScrollEvent {
                                position,
                                delta,
                                modifiers: Modifiers::default(),
                                touch_phase: TouchPhase::Moved,
                                source: ScrollSource::Pointer,
                            }),
                            cx,
                        );
                        window.refresh();
                    });
                    cx.background_executor().timer(Duration::from_millis(16)).await;
                }
                "key" => {
                    if let Some(k) = args.first().and_then(|k| Keystroke::parse(Box::leak(k.to_string().into_boxed_str())).ok()) {
                        let _ = window.update(cx, |_, window, cx| {
                            window.dispatch_keystroke(k, cx);
                            window.refresh();
                        });
                    }
                }
                "type" => {
                    let text = line.strip_prefix("type").unwrap_or("").trim_start().to_string();
                    let _ = window.update(cx, |_, window, cx| {
                        for ch in text.chars() {
                            let key = if ch == ' ' { "space".to_string() } else { ch.to_string() };
                            let stroke = Keystroke {
                                modifiers: Modifiers::default(),
                                key,
                                key_char: Some(ch.to_string()),
                            };
                            window.dispatch_keystroke(stroke, cx);
                        }
                        window.refresh();
                    });
                }
                "shot" | "shotnow" => {
                    // Let a frame render before capturing (`shotnow`: capture immediately).
                    if cmd == "shot" {
                        cx.background_executor().timer(Duration::from_millis(250)).await;
                    }
                    if let Some(shot) = std::env::var_os("SPACE_SHOT_CMD") {
                        let name = args.first().copied().unwrap_or("shot").to_string();
                        let _ = std::process::Command::new(shot).arg(name).status();
                    }
                }
                "resize" => {
                    let _ = window.update(cx, |_, window, _| window.resize(size(px(num(0)), px(num(1)))));
                }
                "size" => {
                    let _ = window.update(cx, |_, window, _| {
                        eprintln!("script: viewport {:?} scale {}", window.viewport_size(), window.scale_factor())
                    });
                }
                "context" => {
                    let _ = window.update(cx, |_, window, _| {
                        let stack: Vec<String> = window.context_stack().iter().map(|c| format!("{c:?}")).collect();
                        eprintln!("script: context {}", stack.join(" > "));
                    });
                }
                "composer" => {
                    let _ = window.update(cx, |root, _, cx| {
                        // The window's root wraps the app's view.
                        let app = root.downcast::<gpui::base::Root>().ok().and_then(|r| r.read(cx).view().clone().downcast::<crate::app::SpaceApp>().ok());
                        if let Some(app) = app {
                            eprintln!("script: composer {:?}", app.read(cx).input.read(cx).value());
                        }
                    });
                }
                "find" => {
                    let _ = window.update(cx, |root, _, cx| {
                        let app = root.downcast::<gpui::base::Root>().ok().and_then(|r| r.read(cx).view().clone().downcast::<crate::app::SpaceApp>().ok());
                        if let Some(app) = app {
                            let app = app.read(cx);
                            let scroll = app.transcript_scroll.scroll_handle();
                            let (open, query, current, matches) = match &app.find {
                                Some(f) => (f.open, f.input.read(cx).value().to_string(), f.current, f.matches.iter().map(|m| (m.ix, m.range.clone())).collect::<Vec<_>>()),
                                None => (false, String::new(), None, Vec::new()),
                            };
                            let expanded = app.current().map(|t| t.expanded.iter().copied().collect::<Vec<_>>()).unwrap_or_default();
                            eprintln!(
                                "script: find open={open} query={query:?} current={current:?} matches={matches:?} expanded={expanded:?} scroll={:.0} max={:.0} following={}",
                                -f32::from(scroll.offset().y),
                                f32::from(scroll.max_offset().y),
                                app.transcript_scroll.is_following(),
                            );
                        }
                    });
                }
                "rename" | "title" => {
                    let typed = line.strip_prefix("rename").unwrap_or("").trim_start().to_string();
                    let rename = cmd == "rename";
                    let _ = window.update(cx, |root, _, cx| {
                        let app = root.downcast::<gpui::base::Root>().ok().and_then(|r| r.read(cx).view().clone().downcast::<crate::app::SpaceApp>().ok());
                        if let Some(app) = app {
                            app.update(cx, |app, cx| {
                                if rename && let Some(key) = app.selected {
                                    app.set_thread_name(key, &typed, cx);
                                }
                                let t = app.current().map(|t| (t.title(), t.renamed));
                                eprintln!("script: title {t:?}");
                            });
                        }
                    });
                }
                "editmenu" => print_edit_menu(),
                "terminals" => {
                    let _ = window.update(cx, |root, _, cx| {
                        let app = root.downcast::<gpui::base::Root>().ok().and_then(|r| r.read(cx).view().clone().downcast::<crate::app::SpaceApp>().ok());
                        if let Some(app) = app {
                            let app = app.read(cx);
                            let pane = app.selected.and_then(|k| app.terminals.panes.get(&k));
                            let clock = space_ui::clock::AnimationClock::global(cx);
                            let (shown, _) = app.terminal_shown.at(clock.now(), clock.pinned());
                            eprintln!("script: terminals {:?} shown {shown:.2}", pane.map(|p| (p.open, p.tabs.len(), p.opening)));
                        }
                    });
                }
                "quit" => {
                    cx.update(|cx| cx.quit());
                }
                other => eprintln!("script: unknown command `{other}`"),
            }
        }
    })
    .detach();
}

/// Each Edit menu item as AppKit sees it: title, action selector, key equivalent and modifiers.
fn print_edit_menu() {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    let menu = NSApplication::sharedApplication(mtm).mainMenu();
    let Some(edit) = menu.and_then(|m| m.itemWithTitle(&NSString::from_str("Edit"))).and_then(|i| i.submenu()) else {
        eprintln!("script: no Edit menu");
        return;
    };
    for item in edit.itemArray() {
        if item.isSeparatorItem() {
            continue;
        }
        let action = item.action().map(|s| s.name().to_string_lossy().into_owned()).unwrap_or_default();
        eprintln!("script: edit {:?} {action} {:?} {:?}", item.title().to_string(), item.keyEquivalent().to_string(), item.keyEquivalentModifierMask());
    }
}
