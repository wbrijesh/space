//! "Agent finished" alerts: a macOS notification (with sound) when you're not looking at the
//! thread, a sound alone when you are. Clicking a notification brings Space forward on that
//! thread.
//!
//! Notifications need a bundled app (`just run` launches `target/debug/Space.app`); a bare
//! binary has no bundle id, so `UNUserNotificationCenter` is unavailable and only the sound
//! plays. The Dock icon is set at launch either way.

use std::sync::OnceLock;

use block2::RcBlock;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSImage, NSSound};
use objc2_foundation::{MainThreadMarker, NSBundle, NSData, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

/// System sound played when a turn finishes while you're watching it.
const DONE_SOUND: &str = "Glass";
const ICON_PNG: &[u8] = include_bytes!("../resources/AppIcon.png");
const ID_PREFIX: &str = "space.thread.";

/// Thread keys from clicked notifications.
static CLICKS: OnceLock<UnboundedSender<u64>> = OnceLock::new();

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SpaceNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// Space is frontmost but on another thread: still show the banner.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            handler.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            handler: &block2::DynBlock<dyn Fn()>,
        ) {
            let id = response.notification().request().identifier().to_string();
            if let Some(key) = id.strip_prefix(ID_PREFIX).and_then(|rest| rest.split('.').next()?.parse().ok())
                && let Some(tx) = CLICKS.get()
            {
                let _ = tx.unbounded_send(key);
            }
            handler.call(());
        }
    }
);

/// Set the Dock icon and, when bundled, register for notifications. Returns the stream of
/// thread keys whose notification was clicked. Call once, on the main thread.
pub fn init() -> UnboundedReceiver<u64> {
    let (tx, rx) = unbounded();
    let _ = CLICKS.set(tx);
    set_dock_icon();
    if let Some(center) = center() {
        let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(), init] };
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The center holds its delegate weakly; keep ours for the life of the process.
        std::mem::forget(delegate);
        let done = RcBlock::new(|granted: Bool, _err: *mut NSError| {
            if !granted.as_bool() {
                tracing::info!("notifications not allowed; finished turns will only play a sound");
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &done,
        );
    }
    rx
}

/// A turn finished. `watching`: Space is frontmost on this thread (sound only).
pub fn turn_finished(thread_key: u64, title: &str, body: &str, watching: bool) {
    match center() {
        Some(center) if !watching => {
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(title));
            content.setBody(&NSString::from_str(if body.is_empty() { "Finished" } else { body }));
            content.setSound(Some(&UNNotificationSound::defaultSound()));
            content.setThreadIdentifier(&NSString::from_str(&format!("{ID_PREFIX}{thread_key}")));
            // Unique per turn, so an older banner for the thread doesn't swallow the new one.
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            let id = NSString::from_str(&format!("{ID_PREFIX}{thread_key}.{nanos}"));
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
            center.addNotificationRequest_withCompletionHandler(&request, None);
        }
        _ => play_sound(),
    }
}

fn play_sound() {
    if let Some(sound) = NSSound::soundNamed(&NSString::from_str(DONE_SOUND)) {
        sound.play();
    }
}

/// The notification center, when running as a bundle (it raises on a bare binary).
fn center() -> Option<Retained<UNUserNotificationCenter>> {
    NSBundle::mainBundle().bundleIdentifier()?;
    Some(UNUserNotificationCenter::currentNotificationCenter())
}

fn set_dock_icon() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(ICON_PNG);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    }
}

/// Whether Space is the active app.
pub fn app_active() -> bool {
    MainThreadMarker::new().is_some_and(|mtm| NSApplication::sharedApplication(mtm).isActive())
}
