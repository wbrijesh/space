//! Undo and Redo in native text fields (the rename sheet, the field in an open panel).
//!
//! Cut, Copy, Paste and Select All reach a native field through the Edit menu's items, which
//! carry AppKit's own actions. GPUI's Undo and Redo items can't: they always run Space's own
//! Undo/Redo, so ⌘Z in the rename sheet would undo in the composer behind it instead. While a
//! native field is being edited, ⌘Z and ⇧⌘Z are sent to it here, before the menu sees them.

use std::ptr::{self, NonNull};

use block2::RcBlock;
use objc2::runtime::Sel;
use objc2::{ClassType as _, MainThreadMarker, sel};
use objc2_foundation::NSObjectProtocol as _;
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSText};

/// Start sending ⌘Z and ⇧⌘Z to native text fields. Call once, at launch.
pub(crate) fn init() {
    let handler = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit passes a live event for the duration of the call.
        let e = unsafe { event.as_ref() };
        if let (Some(action), Some(mtm)) = (undo_action(e), MainThreadMarker::new()) {
            let app = NSApplication::sharedApplication(mtm);
            if editing_native_text(&app) {
                // SAFETY: a nil target sends the action along the key window's responder chain.
                unsafe { app.sendAction_to_from(action, None, None) };
                return ptr::null_mut();
            }
        }
        event.as_ptr()
    });
    // SAFETY: the handler returns the event or nil, as a local monitor must.
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler) };
    // The monitor stays for the life of the app.
    std::mem::forget(monitor);
}

/// `undo:` for ⌘Z and `redo:` for ⇧⌘Z.
fn undo_action(e: &NSEvent) -> Option<Sel> {
    let flags = e.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask;
    let key = e.charactersIgnoringModifiers()?.to_string().to_lowercase();
    if key != "z" {
        return None;
    }
    if flags == NSEventModifierFlags::Command {
        Some(sel!(undo:))
    } else if flags == NSEventModifierFlags::Command | NSEventModifierFlags::Shift {
        Some(sel!(redo:))
    } else {
        None
    }
}

/// Whether the key window is editing a native text field: its first responder is AppKit's
/// field editor (an `NSText`). Space's own windows never are.
fn editing_native_text(app: &NSApplication) -> bool {
    app.keyWindow().and_then(|w| w.firstResponder()).is_some_and(|r| r.isKindOfClass(NSText::class()))
}
