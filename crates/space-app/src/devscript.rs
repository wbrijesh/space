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
//! scroll 400 300 -120 trackpad scroll at (400, 300) moving the view 120 px (negative: up)
//! key cmd-n           dispatch a keystroke
//! type some text      type text into the focused element
//! resize 1280 820     resize the window (logical px)
//! context             print the focused element's key context stack
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
                "move" | "click" => {
                    let position = point(px(num(0)), px(num(1)));
                    let click = cmd == "click";
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
                                    button: MouseButton::Left,
                                    position,
                                    modifiers: Modifiers::default(),
                                    click_count: 1,
                                    first_mouse: false,
                                }),
                                cx,
                            );
                            window.dispatch_event(
                                PlatformInput::MouseUp(MouseUpEvent {
                                    button: MouseButton::Left,
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
                "quit" => {
                    cx.update(|cx| cx.quit());
                }
                other => eprintln!("script: unknown command `{other}`"),
            }
        }
    })
    .detach();
}
