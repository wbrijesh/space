//! A native macOS sheet that asks for one line of text: an `NSAlert` with a text field under its
//! message, slid down from the window like GPUI's own prompts (which can only show buttons).
//!
//! The field starts with `initial`, all selected. Return takes the first button and Esc the
//! second (Cancel); the first button is disabled while the field is empty or only spaces, so
//! an empty answer can't be given.

use std::cell::Cell;
use std::ptr::NonNull;

use block2::RcBlock;
use futures::channel::oneshot;
use gpui::Window;
use objc2::{MainThreadMarker, MainThreadOnly as _};
use objc2::rc::Retained;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSControlTextDidChangeNotification, NSModalResponse, NSTextField, NSView, NSWindow};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSPoint, NSRect, NSSize, NSString};
use raw_window_handle::RawWindowHandle;

/// Width of the text field (the alert sizes itself around it).
const FIELD_W: f64 = 300.;

/// What a text prompt asks: its title, the line under it, the field's text, and the button that
/// accepts it.
pub(crate) struct TextPrompt<'a> {
    pub title: &'a str,
    pub detail: &'a str,
    pub initial: &'a str,
    pub accept: &'a str,
}

/// Show the sheet on `window`. The answer is the field's text (trimmed, never empty), or `None`
/// when cancelled; the receiver fails if the sheet couldn't be shown.
pub(crate) fn ask(window: &Window, prompt: TextPrompt<'_>) -> oneshot::Receiver<Option<String>> {
    let (tx, rx) = oneshot::channel();
    let Some(mtm) = MainThreadMarker::new() else { return rx };
    let Some(ns_window) = ns_window(window) else {
        tracing::warn!("text prompt: no native window");
        return rx;
    };

    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Informational);
    alert.setMessageText(&NSString::from_str(prompt.title));
    if !prompt.detail.is_empty() {
        alert.setInformativeText(&NSString::from_str(prompt.detail));
    }
    let accept = alert.addButtonWithTitle(&NSString::from_str(prompt.accept));
    let cancel = alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    cancel.setKeyEquivalent(&NSString::from_str("\u{1b}"));

    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), NSRect::new(NSPoint::new(0., 0.), NSSize::new(FIELD_W, 24.)));
    field.setStringValue(&NSString::from_str(prompt.initial));
    field.setUsesSingleLineMode(true);
    alert.setAccessoryView(Some(&field));
    alert.layout();
    alert.window().setInitialFirstResponder(Some(&field));
    accept.setEnabled(!prompt.initial.trim().is_empty());

    // Keep the accept button disabled while the field is blank.
    let observer = {
        let (watched, accept) = (field.clone(), accept.clone());
        let block = RcBlock::new(move |_: NonNull<NSNotification>| {
            accept.setEnabled(!watched.stringValue().to_string().trim().is_empty());
        });
        // SAFETY: the name is a constant AppKit defines; the block lives as long as the observer.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(NSControlTextDidChangeNotification),
                Some(&field),
                None,
                &block,
            )
        }
    };

    let tx = Cell::new(Some(tx));
    let done = {
        let field = field.clone();
        let observer = Cell::new(Some(observer));
        RcBlock::new(move |answer: NSModalResponse| {
            if let Some(observer) = observer.take() {
                // SAFETY: removes the observer added above.
                unsafe { NSNotificationCenter::defaultCenter().removeObserver(observer.as_ref()) };
            }
            let text = field.stringValue().to_string();
            let text = text.trim();
            let answer = (answer == NSAlertFirstButtonReturn && !text.is_empty()).then(|| text.to_string());
            if let Some(tx) = tx.take() {
                let _ = tx.send(answer);
            }
        })
    };
    // As GPUI does for its prompts: begin the sheet on the next turn of the run loop, not while
    // AppKit is still dispatching the key or click that asked for it.
    let alert: Retained<NSAlert> = alert;
    let block = done;
    let queued = RcBlock::new(move || alert.beginSheetModalForWindow_completionHandler(&ns_window, Some(&block)));
    run_on_main_queue(queued);
    rx
}

/// The window's `NSWindow`, through its content view.
pub(crate) fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
    // SAFETY: GPUI's AppKit window hands out its live content view; it outlives this call.
    let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    view.window()
}

/// Run `block` on the main queue, after the current event.
fn run_on_main_queue(block: RcBlock<dyn Fn()>) {
    use objc2_foundation::NSRunLoop;
    // SAFETY: `performBlock:` copies the block and runs it on this (the main) run loop.
    unsafe { NSRunLoop::mainRunLoop().performBlock(&block) };
}
