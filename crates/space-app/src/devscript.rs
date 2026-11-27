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
//! click 120 300 [2]   move + left mouse down + up (2: a double click, 3: a triple click)
//! rclick 120 300      the same with the right button (context menus)
//! drag 100 300 400 300  press at the first point, move to the second (frame by frame), release
//! press 100 300 [2] [shift]  left mouse down, the nth click, with shift (held: `moveto` drags, `release` lets go)
//! moveto 400 300      move with the left button held, in 8 steps from where the last press or move was
//! release 400 300     left mouse up
//! selection           print the window's selected text and where it ends, and the quote state
//! scroll 400 300 -120 trackpad scroll at (400, 300) moving the view 120 px (negative: up)
//! swipe 400 300 -200 [30]  a two-finger trackpad gesture at (400, 300): began, moved 200 px across
//!                     (and 30 down) as the platform reports it (with natural scrolling, negative:
//!                     fingers to the left), ended, then a little momentum
//! key cmd-n           dispatch a keystroke
//! modifiers cmd       hold these modifiers (`cmd`, `alt`, `ctrl`, `shift`, joined by `-`; `none` lets go)
//! type some text      type text into the focused element
//! resize 1280 820     resize the window (logical px)
//! size                print the window's size and scale factor
//! context             print the focused element's key context stack
//! composer            print the composer's text and its editor's width (one row and two rows differ)
//! find                print find's state (query, matches, current) and the transcript's scroll
//! rename some name    rename the selected thread as the Rename sheet would (then print its title)
//! title               print the selected thread's title
//! editmenu            print the Edit menu's items: AppKit action and key equivalent
//! sidechat            print the selected thread's side chat: focused, its input, the composer's, items, running
//! trajectory          print whether the selected thread shows its trajectory, and what has focus
//! terminals           print the selected thread's right pane: open, its view, tabs, shells starting, how far it's
//!                     shown, whether it's maximized, its set width; and whether the sidebar is open
//! open src/main.rs    show a file (relative to the thread's folder) in the Files view, as a click on its row would
//! changes             print the selected thread's Changes bar: status, main step, ▾ steps, files, the open one, error, done
//! rewind              print the selected thread's going-back state: its messages (index, id, can go back), the
//!                     fork waiting, rewinds that can be undone, the editor, rewind mode, the notice
//! edit 3              start editing the 3rd message of yours (from 1), as ✎ would
//! rewindto 3          rewind to the 3rd message of yours, as ↺ would
//! undo                undo the last rewind, as ⌘Z or the notice's Undo would
//! shot name           capture this window to `$SPACE_SHOT_DIR/name.png` (or run `$SPACE_SHOT_CMD name`)
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
        // Where the held mouse is (`press`, `moveto`).
        let mut held = point(px(0.), px(0.));
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
                        let clicks = if click { (args.get(2).and_then(|n| n.parse::<usize>().ok())).unwrap_or(1) } else { 0 };
                        for click_count in 1..=clicks {
                            window.dispatch_event(
                                PlatformInput::MouseDown(MouseDownEvent {
                                    button,
                                    position,
                                    modifiers: Modifiers::default(),
                                    click_count,
                                    first_mouse: false,
                                }),
                                cx,
                            );
                            window.dispatch_event(
                                PlatformInput::MouseUp(MouseUpEvent { button, position, modifiers: Modifiers::default(), click_count }),
                                cx,
                            );
                        }
                        window.refresh();
                    });
                    cx.background_executor().timer(Duration::from_millis(16)).await;
                }
                "drag" => {
                    let (from, to) = (point(px(num(0)), px(num(1))), point(px(num(2)), px(num(3))));
                    let input = |e: PlatformInput| {
                        move |window: &mut Window, cx: &mut App| {
                            window.dispatch_event(e, cx);
                            window.refresh();
                        }
                    };
                    let left = MouseButton::Left;
                    let none = Modifiers::default();
                    let mut events = vec![
                        PlatformInput::MouseMove(MouseMoveEvent { position: from, pressed_button: None, modifiers: none }),
                        PlatformInput::MouseDown(MouseDownEvent { button: left, position: from, modifiers: none, click_count: 1, first_mouse: false }),
                    ];
                    for i in 1..=8 {
                        let position = from + (to - from) * (i as f32 / 8.);
                        events.push(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: Some(left), modifiers: none }));
                    }
                    events.push(PlatformInput::MouseUp(MouseUpEvent { button: left, position: to, modifiers: none, click_count: 1 }));
                    for e in events {
                        let _ = window.update(cx, |_, window, cx| input(e)(window, cx));
                        cx.background_executor().timer(Duration::from_millis(20)).await;
                    }
                }
                "press" | "moveto" | "release" => {
                    let to = point(px(num(0)), px(num(1)));
                    let left = MouseButton::Left;
                    let none = Modifiers::default();
                    let shift = Modifiers { shift: args.contains(&"shift"), ..Default::default() };
                    let count = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(1);
                    let events = match cmd {
                        "press" => vec![
                            PlatformInput::MouseMove(MouseMoveEvent { position: to, pressed_button: None, modifiers: shift }),
                            PlatformInput::MouseDown(MouseDownEvent { button: left, position: to, modifiers: shift, click_count: count, first_mouse: false }),
                        ],
                        "moveto" => (1..=8)
                            .map(|i| {
                                let position = held + (to - held) * (i as f32 / 8.);
                                PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: Some(left), modifiers: none })
                            })
                            .collect(),
                        _ => vec![PlatformInput::MouseUp(MouseUpEvent { button: left, position: to, modifiers: none, click_count: count })],
                    };
                    held = to;
                    for e in events {
                        let _ = window.update(cx, |_, window, cx| window.dispatch_event(e, cx));
                        cx.background_executor().timer(Duration::from_millis(20)).await;
                    }
                }
                "selection" => {
                    let _ = window.update(cx, |root, _, cx| {
                        let app = root.downcast::<gpui::base::Root>().ok().and_then(|r| r.read(cx).view().clone().downcast::<crate::app::SpaceApp>().ok());
                        if let Some(app) = app {
                            let app = app.read(cx);
                            let draft = app.selected.and_then(|k| app.thread(k)).and_then(|t| t.quote.clone());
                            eprintln!("script: quote draft={draft:?} offer={:?}", app.quote_offer.as_ref().map(|(_, t)| t));
                        }
                    });
                    let _ = window.update(cx, |_, window, cx| {
                        let end = gpui::base::TextSelection::end_caret(window, cx);
                        eprintln!("script: selection {:?} end={end:?}", gpui::base::TextSelection::selected_text(window, cx));
                    });
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
                "swipe" => {
                    let position = point(px(num(0)), px(num(1)));
                    let steps = 8;
                    let (dx, dy) = (num(2) / steps as f32, num(3) / steps as f32);
                    let mut phases = vec![(TouchPhase::Started, 0., 0.)];
                    phases.extend((0..steps).map(|_| (TouchPhase::Moved, dx, dy)));
                    phases.push((TouchPhase::Ended, 0., 0.));
                    // Momentum after the fingers lift (macOS sends it without a phase: `Moved`).
                    phases.extend((1..=4).map(|i| (TouchPhase::Moved, dx / (i as f32 * 2.), dy / (i as f32 * 2.))));
                    for (touch_phase, dx, dy) in phases {
                        let _ = window.update(cx, |_, window, cx| {
                            window.dispatch_event(
                                PlatformInput::Scroll(ScrollEvent {
                                    position,
                                    delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
                                    modifiers: Modifiers::default(),
                                    touch_phase,
                                    source: ScrollSource::Pointer,
                                }),
                                cx,
                            );
                            window.refresh();
                        });
                        cx.background_executor().timer(Duration::from_millis(16)).await;
                    }
                }
                "key" => {
                    if let Some(k) = args.first().and_then(|k| Keystroke::parse(Box::leak(k.to_string().into_boxed_str())).ok()) {
                        let _ = window.update(cx, |_, window, cx| {
                            window.dispatch_keystroke(k, cx);
                            window.refresh();
                        });
                    }
                }
                "modifiers" => {
                    let held = args.first().copied().unwrap_or("none");
                    let has = |m: &str| held.split('-').any(|h| h == m);
                    let modifiers = Modifiers { platform: has("cmd"), alt: has("alt"), control: has("ctrl"), shift: has("shift"), function: false };
                    let _ = window.update(cx, |_, window, cx| {
                        window.dispatch_event(PlatformInput::ModifiersChanged(ModifiersChangedEvent { modifiers, capslock: Capslock::default() }), cx);
                        window.refresh();
                    });
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
                    // Let a frame render before capturing (`shotnow`: capture immediately). A
                    // hidden window (another workspace) draws no frames, but a resize draws one (a taller one:
                    // narrowing would close panels, see `fit`).
                    if cmd == "shot" {
                        let shown = window.update(cx, |_, window, _| window.viewport_size()).ok();
                        if let Some(s) = shown {
                            let _ = window.update(cx, |_, window, _| window.resize(size(s.width, s.height + px(1.))));
                            cx.background_executor().timer(Duration::from_millis(100)).await;
                            let _ = window.update(cx, |_, window, _| window.resize(s));
                        }
                        cx.background_executor().timer(Duration::from_millis(250)).await;
                    }
                    let name = args.first().copied().unwrap_or("shot").to_string();
                    if let Some(dir) = std::env::var_os("SPACE_SHOT_DIR") {
                        // The window itself, by its window number (no shadow), even when it's behind
                        // others or on another workspace (see scripts/capture-window.swift).
                        let number = window.update(cx, |_, window, _| crate::text_prompt::ns_window(window).map(|w| w.windowNumber())).ok().flatten();
                        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
                        match (number, capture_window()) {
                            (None, _) => eprintln!("script: shot: no native window"),
                            (_, Err(e)) => eprintln!("script: shot failed: {e}"),
                            (Some(n), Ok(capture)) => {
                                let out = std::process::Command::new(capture).arg(n.to_string()).arg(&path).output();
                                match out {
                                    Ok(o) if o.status.success() => eprintln!("script: shot {}", path.display()),
                                    Ok(o) => eprintln!("script: shot failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
                                    Err(e) => eprintln!("script: shot failed: {e}"),
                                }
                            }
                        }
                    } else if let Some(shot) = std::env::var_os("SPACE_SHOT_CMD") {
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
                            let input = app.read(cx).input.read(cx);
                            eprintln!("script: composer {:?} width {:.0}", input.value(), f32::from(input.input_bounds().size.width));
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
                "sidechat" => {
                    let _ = window.update(cx, |root, window, cx| {
                        let app = root.downcast::<gpui::base::Root>().ok().and_then(|r| r.read(cx).view().clone().downcast::<crate::app::SpaceApp>().ok());
                        if let Some(app) = app {
                            let app = app.read(cx);
                            let Some(main) = app.selected else { return };
                            let composer = app.input.read(cx).value().to_string();
                            let count = app.side_chats.values().filter(|s| s.thread.side_of == Some(main)).count();
                            // The one shown, else the newest.
                            let shown = match app.terminals.pane(main).and_then(|p| p.active_tab()) {
                                Some(crate::pane::PaneTab::SideChat(k)) => Some(*k),
                                _ => app.side_chats.values().filter(|s| s.thread.side_of == Some(main)).max_by_key(|s| s.thread.created_ms).map(|s| s.thread.key),
                            };
                            match shown.and_then(|k| app.side_chats.get(&k)) {
                                Some(side) => eprintln!(
                                    "script: sidechat count={count} title={:?} focused={} input={:?} composer={composer:?} items={} running={} error={:?}",
                                    side.title(),
                                    app.side_chat_focused(side.thread.key, window, cx),
                                    side.input.read(cx).value().to_string(),
                                    side.thread.state.items.len(),
                                    side.thread.state.running,
                                    side.thread.error,
                                ),
                                None => eprintln!("script: sidechat none composer={composer:?}"),
                            }
                        }
                    });
                }
                "trajectory" => {
                    let _ = window.update(cx, |root, window, cx| {
                        let app = root.downcast::<gpui::base::Root>().ok().and_then(|r| r.read(cx).view().clone().downcast::<crate::app::SpaceApp>().ok());
                        if let Some(app) = app {
                            let app = app.read(cx);
                            eprintln!(
                                "script: trajectory shown={} available={} focused={} composer_focused={}",
                                app.trajectory_shown(),
                                app.host.has_trajectory(),
                                app.trajectories.focus.contains_focused(window, cx),
                                app.input.read(cx).focus_handle(cx).is_focused(window),
                            );
                        }
                    });
                }
                "terminals" => {
                    let _ = window.update(cx, |root, _, cx| {
                        let app = root.downcast::<gpui::base::Root>().ok().and_then(|r| r.read(cx).view().clone().downcast::<crate::app::SpaceApp>().ok());
                        if let Some(app) = app {
                            let app = app.read(cx);
                            let pane = app.selected.and_then(|k| app.terminals.panes.get(&k));
                            let clock = space_ui::clock::AnimationClock::global(cx);
                            let (shown, _) = app.terminal_shown.at(clock.now(), clock.pinned());
                            eprintln!(
                                "script: terminals {:?} maximized {} shown {shown:.2} width {} sidebar {}",
                                pane.map(|p| (p.open, p.active_tab().cloned(), p.tabs.len())),
                                pane.is_some_and(|p| p.maximized),
                                app.terminals.width,
                                app.sidebar_open,
                            );
                        }
                    });
                }
                "threads" => {
                    let _ = window.update(cx, |root, _, cx| {
                        let app = root.downcast::<gpui::base::Root>().ok().and_then(|r| r.read(cx).view().clone().downcast::<crate::app::SpaceApp>().ok());
                        if let Some(app) = app {
                            let app = app.read(cx);
                            let titles: Vec<String> = app
                                .threads
                                .iter()
                                .map(|t| format!("{}{}{}", if app.selected == Some(t.key) { "*" } else { "" }, t.title(), if t.done_ms.is_some() { " (reading, done)" } else { "" }))
                                .collect();
                            let done: Vec<&str> = app.done_threads.iter().map(|m| m.title.as_str()).collect();
                            let notice = app.rewind.notice.as_ref().map(|n| (n.title.clone(), n.body.clone()));
                            eprintln!("script: threads {titles:?} done {done:?} notice {notice:?} undo {}", app.rewind.done.is_some());
                        }
                    });
                }
                "open" => {
                    let rel = line.strip_prefix("open").unwrap_or("").trim().to_string();
                    let _ = window.update(cx, |root, window, cx| {
                        let app = root.downcast::<gpui::base::Root>().ok().and_then(|r| r.read(cx).view().clone().downcast::<crate::app::SpaceApp>().ok());
                        if let Some(app) = app {
                            app.update(cx, |app, cx| {
                                let Some(key) = app.selected else { return };
                                let Some(cwd) = app.thread(key).map(|t| t.cwd.clone()) else { return };
                                let path = cwd.canonicalize().unwrap_or(cwd).join(&rel);
                                app.show_file(key, &path, window, cx);
                            });
                        }
                    });
                }
                "rewind" | "edit" | "rewindto" | "undo" => {
                    let nth: Option<usize> = args.first().and_then(|a| a.parse().ok());
                    let _ = window.update(cx, |root, window, cx| {
                        let app = root.downcast::<gpui::base::Root>().ok().and_then(|r| r.read(cx).view().clone().downcast::<crate::app::SpaceApp>().ok());
                        if let Some(app) = app {
                            app.update(cx, |app, cx| {
                                let ix = app.current().and_then(|t| {
                                    let users: Vec<usize> = t.state.items.iter().enumerate().filter(|(_, i)| matches!(i, space_core::TranscriptItem::User { .. })).map(|(ix, _)| ix).collect();
                                    users.get(nth?.checked_sub(1)?).copied()
                                });
                                match (cmd, ix) {
                                    ("edit", Some(ix)) => app.start_edit(ix, window, cx),
                                    ("rewindto", Some(ix)) => app.rewind_at(ix, window, cx),
                                    ("undo", _) => {
                                        if let Some(key) = app.selected {
                                            app.undo_rewind(key, cx);
                                        }
                                    }
                                    _ => {}
                                }
                                eprintln!("script: rewind {}", app.rewind_summary(cx));
                            });
                        }
                    });
                }
                "changes" => {
                    let _ = window.update(cx, |root, _, cx| {
                        let app = root.downcast::<gpui::base::Root>().ok().and_then(|r| r.read(cx).view().clone().downcast::<crate::app::SpaceApp>().ok());
                        if let Some(app) = app {
                            eprintln!("script: changes {}", app.read(cx).changes_summary(cx));
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

/// The window-capture helper, compiled from scripts/capture-window.swift the first time (and
/// again when the source is newer), into the build folder.
fn capture_window() -> Result<std::path::PathBuf, String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = root.join("scripts/capture-window.swift");
    let out = root.join("target/debug/capture-window");
    let modified = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    if modified(&out).is_some_and(|built| modified(&source).is_none_or(|src| src <= built)) {
        return Ok(out);
    }
    let built = std::process::Command::new("swiftc").arg("-O").arg(&source).arg("-o").arg(&out).output().map_err(|e| format!("swiftc: {e}"))?;
    if !built.status.success() {
        return Err(format!("compiling {}: {}", source.display(), String::from_utf8_lossy(&built.stderr).trim()));
    }
    Ok(out)
}
