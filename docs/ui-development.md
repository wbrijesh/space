# UI development

How Space's UI is built and how to work on it. The short version: **tokens → components with
previews → gallery → scenarios**, with one **animation clock** under all motion. The goal is that
you can put any component, or the whole app, into a specific state and have it *stay* there.

```
crates/space-ui      tokens, clock, components (+ previews, gallery behind the `dev` feature)
crates/space-app     app state + wiring (view.rs), scenarios, dev menu  (feature `dev`, on by default)
crates/space-core    stub agent + stub_script (scripted, holdable turns)
```

## 1. Design tokens — `crates/space-ui/src/theme/`

Every color, size, radius, font and timing comes from here. Components never use raw literals
for these.

| Module | Contents |
|---|---|
| `color` | Palette as functions (`color::text()`, `color::accent()`); `color::ALL` lists them with hex and purpose |
| `space` | 4px spacing scale for computed values. Plain layout uses GPUI's 4px helpers (`gap_2` = 8px, `px_3` = 12px), the same scale |
| `size` | Control heights, icon sizes, layout widths (`size::THREAD_ROW_H`, `size::SPINNER_LG`, …) |
| `radius` | Corner radii (`radius::MD` for buttons and rows, `radius::CARD` for the composer) |
| `text` | Type scale (`text::MD`, `text::BODY`, …), line heights, `UI_FONT` / `MONO_FONT` |
| `motion` | Animation cycles and curves: spinner (1.6 s, 8 spokes), dots (1.3 s, 16% stagger), toggle slide (180 ms, ease-out), scroll glide (200 ms), button pop (160 ms, from 0.92) |

`theme::install` maps the tokens onto gpui-component's theme, so stock widgets (textarea, popup
menus, tooltips, scrollbars) match. The gallery's **Tokens** page shows all of them.

## 2. Components — `crates/space-ui/src/components/`

A component takes **data and callbacks, never app state**. It is either a `RenderOnce` struct
with builder methods (`ThreadRow::new(id, title).selected(true).on_click(…)`) or a function
returning an element (`tool_row(ix, &call, open, on_toggle)`). The app's `view.rs` maps
`SpaceApp` state to props and wires callbacks back to `SpaceApp` methods. The gallery passes
fixture data and no-op callbacks. Both render **the same code**.

Conventions:

* One file per area (`thread_row.rs`, `composer.rs`, `transcript.rs`, …). Small shared pieces go
  in `widgets.rs`.
* Clickable chrome is a styled `div`, not a focusable button, so mouse clicks never leave focus
  rings behind.
* State a component needs across frames (drag tracking, the composer's layout mode) lives in keyed
  element state (`window.use_keyed_state`), not in the app.
* Anything animated reads time from the clock (below), never from wall-clock time.

## 3. Previews (fixtures) — next to each component

Each component module ends with, behind the `dev` feature:

```rust
#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    vec![
        ComponentPreview::new(Area::Sidebar, "Thread row", "One line per thread with an activity indicator.")
            .stacked()
            .examples([
                Example::new("selected", |_, _| ThreadRow::new("a", "Fix the smoke test").selected(true).into_any_element())
                    .width(size::SIDEBAR_W),
                // … one Example per named state
            ]),
    ]
}
```

`crate::preview::registry()` concatenates every module's `previews()`. **Adding a component:**
write the component in its own module under `components/` (props + callbacks, tokens only), add
a `previews()` that lists its named states (idle / selected / working / error …), and add the
module's `previews()` to `registry()` in `preview.rs`. A unit test checks the registry: every
area populated, no duplicate names, every preview has examples. Fixtures that need entities (a
textarea, an open popup menu) create them with `window.use_keyed_state(key, …)`, so the state
persists across frames. See `components/composer.rs`.

## 4. Gallery — dev-only window

⌥⌘G, Dev ▸ Component Gallery, `Space --gallery`, or `just gallery`. On the left are components
grouped by area (Shell, Sidebar, Composer, Transcript, Settings, Indicators) and the Tokens page. The main
pane shows each example with its name. The top bar has:

* **Animation clock:** Live / Pause / Step (one 60 Hz frame), speed 0.1× / 0.25× / 1×, and
  Pin phase with a slider.
* **Canvas:** window background or a raised surface.

`SPACE_GALLERY_PAGE=<component name | tokens>` opens a page directly; `SPACE_GALLERY_ONLY=<text>|<text>` keeps only
the examples whose name contains one of them (handy for screenshots further down a long page).

## 5. Scenarios — the real app in a held state

`Space --scenario <name>` (or `just scenario name=<name>`, or Dev ▸ Relaunch in Scenario) boots
the **real app shell** against the stub agent playing a script (`space_core::stub_script`).
Script steps can `hold`, so a turn stays thinking, running a tool, or waiting for permission
until you cancel it. Nothing in the UI is faked. `Space --scenario list` prints them:

| Scenario | State |
|---|---|
| `empty` | one empty thread |
| `thread-working` | reply streaming, held mid-sentence |
| `thread-thinking` | held while thinking |
| `tool-running` | held while a tool runs |
| `permission-pending` | waiting for Allow / Reject |
| `error` | failed turn |
| `many-threads` | nine threads in mixed states (working, waiting, unread, error) |
| `long-composer` | finished thread with a 12-line draft |
| `long-transcript` | thread longer than the window, scrolled to the end (bottom fade) |
| `scrolled-up` | long thread held scrolled up (scroll-to-bottom button) while the reply keeps streaming below |
| `guardrail` | the real permissions plugin without a key: a Read Only thread with a blocked edit, a Workspace Write thread paused |
| `guardrail-offline` | the real permissions plugin with a key but OpenAI unreachable: a paused call, the retrying pill with live times |
| `settings` | the Settings page, with the stub's models and a saved default model and thinking level |

**Adding a scenario:** in `crates/space-app/src/scenarios.rs`, append a `Scenario` to `all()`.
Give it a name, a description and its threads. Each thread has a `prompt` and the `steps` the
stub streams back (`thought`, `say`, `tool(...)`, `Step::Permission`, `context(...)`,
`Step::Hold`, `Step::Fail`). Optionally set a `subdir`, a composer `draft` or a permission `mode` (with `permissions: true`). Prompts must be
unique within a scenario (the stub matches on prompt text); a unit test checks this. The stub
binary must be built (`cargo build --workspace` does it).

## 6. Animation clock — `crates/space-ui/src/clock.rs`

`AnimationClock` is a GPUI global that every animation reads:

```rust
let phase = AnimationClock::phase_for(motion::SPINNER_CYCLE, window, cx); // 0..1
```

It supports these modes: live (default), paused, speed multiplier, pinned phase (all animations
show that phase), and single-frame step. Components request animation frames only while the
clock is running, and render a still frame under the system's reduced-motion setting.

* **Dev shortcuts:**
  * ⌥⌘P pause/resume
  * ⌥⌘. step
  * ⌥⌘1 / ⌥⌘2 / ⌥⌘3 for 0.1× / 0.25× / 1×
  * ⌥⌘0 reset

  The same commands are in the Dev menu.
* **`SPACE_CLOCK` at launch:** `paused`, `pin:0.35` or `speed:0.25`.

Pinned and paused frames are pixel-stable. A scenario captured twice, 2 s apart, with
`SPACE_CLOCK=pin:0.3` gives identical images, apart from the text caret, whose blink belongs to
the textarea.

## Dev vs. release

Dev tooling (previews, gallery, scenarios, clock controls, `SPACE_SCRIPT` input replay) is behind
the `dev` cargo feature. space-app enables it by default, so every `cargo run` has it.
`just release` builds with `--no-default-features`, and none of it is compiled in. A feature was
chosen over `cfg(debug_assertions)` for two reasons. It keeps the choice explicit, and the gallery
stays available in optimized builds for profiling. CI and lint run both configurations
(`cargo clippy -p space-app --no-default-features`).

## Interaction scripts

`SPACE_SCRIPT=<file>` replays clicks, scrolls, keys and typing through GPUI's own event
dispatch. The commands are `click X Y`, `move X Y`, `scroll X Y DY` (trackpad scroll; negative DY
moves the view up), `key cmd-n`, `type text`, `wait MS`, `shot NAME` and `shotnow NAME`; `shot` and
`shotnow` run `$SPACE_SHOT_CMD NAME`. Use it for interaction checks that need no OS input
permissions (see `crates/space-app/src/devscript.rs`). While a script runs, the app logs
`script: transcript following=…` when the transcript stops or resumes following its bottom.

## Inspired by

Zed's component preview (components registering example states that an in-app window renders),
SwiftUI `#Preview` (previews written next to the view), and Storybook stories (named states per
component, plus a tokens page). Scenarios borrow from fixture-backed app states: the real shell
over a deterministic backend.
