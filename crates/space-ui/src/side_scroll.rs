//! Content that scrolls sideways (a file's long lines, a diff, the right pane's tabs) says so,
//! for the two-finger swipe that shows and hides a pane (space-app's `swipe.rs`): a gesture that
//! begins where content can still scroll its way scrolls that content instead of swiping, as
//! Safari's back and forward swipe does.
//!
//! The scroller adds [`mark`] as a child (over its whole area); each frame the window's root
//! calls [`new_frame`] first, and the swipe asks [`can_scroll`] for the point under the pointer.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::component::input::EditorState;
use gpui::*;

/// Room closer than this counts as none, so a sub-pixel residue at an edge isn't room to scroll.
const EPSILON: f32 = 0.5;

/// How far a scroller can still move from where it is: toward the start of its content (a
/// positive sideways delta, which adds to the offset) and toward its end (a negative one).
pub type Room = Rc<dyn Fn(&App) -> (Pixels, Pixels)>;

/// This frame's marked scrollers in each window that asks, front-most last.
#[derive(Default)]
struct Marks(HashMap<WindowId, Vec<(HitboxId, Room)>>);

impl Global for Marks {}

/// Forget the last frame's marks: call it from the root, before anything paints (prepaint).
/// Only windows that call it take marks.
pub fn new_frame(window: &Window, cx: &mut App) {
    cx.default_global::<Marks>().0.entry(window.window_handle().window_id()).or_default().clear();
}

/// Marks its parent's area (give the parent `.relative()`) as content that scrolls sideways by
/// as much as `room` says.
pub fn mark(room: Room) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, cx| {
            let id = window.window_handle().window_id();
            if cx.has_global::<Marks>()
                && let Some(marks) = cx.global_mut::<Marks>().0.get_mut(&id)
            {
                marks.push((hitbox.id, room));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Whether the content under the pointer would scroll for a sideways delta of `dx`: it scrolls
/// sideways and has room left that way.
pub fn can_scroll(dx: f32, window: &Window, cx: &App) -> bool {
    let id = window.window_handle().window_id();
    let Some(marks) = cx.try_global::<Marks>().and_then(|m| m.0.get(&id)) else { return false };
    let Some((_, room)) = marks.iter().rev().find(|(hitbox, _)| hitbox.should_handle_scroll(window)) else {
        return false;
    };
    let (start, end) = room(cx);
    f32::from(if dx > 0. { start } else { end }) > EPSILON
}

/// A read-only editor's room (a file or a diff): none when its lines wrap.
pub fn editor_room(editor: &Entity<EditorState>) -> Room {
    let editor = editor.downgrade();
    Rc::new(move |cx| editor.upgrade().map_or((px(0.), px(0.)), |e| e.read(cx).horizontal_scroll_room()))
}

/// The room of an element that scrolls sideways with `handle` (`track_scroll`).
pub fn handle_room(handle: &ScrollHandle) -> Room {
    let handle = handle.clone();
    Rc::new(move |_| {
        let x = handle.offset().x;
        ((-x).max(px(0.)), (handle.max_offset().x + x).max(px(0.)))
    })
}
