//! Notifications plugins ask for (`host/notify`; the alerts plugin decides when): a macOS
//! notification, or the sound alone. Clicking a notification brings Space forward on its
//! thread. Also: the notification permission status for the Settings page, and asking for it.
//!
//! Notifications need a bundled app (`just run` launches `target/debug/Space.app`); a bare
//! binary has no bundle id, so `UNUserNotificationCenter` is unavailable and only the sound
//! plays. The Dock icon is set at launch either way.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use block2::RcBlock;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSImage, NSSound};
use objc2_foundation::{MainThreadMarker, NSBundle, NSData, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSettings, UNNotificationSound,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

pub use space_ui::components::settings::NotificationPermission;

/// System sound played for an alert without a banner.
const DONE_SOUND: &str = "Glass";
const ICON_PNG: &[u8] = include_bytes!("../resources/AppIcon.png");
const ID_PREFIX: &str = "space.thread.";

/// System Settings ▸ Notifications ▸ Space.
const SYSTEM_SETTINGS_URL: &str = "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=dev.brijesh.space";

/// Thread keys from clicked notifications.
static CLICKS: OnceLock<UnboundedSender<u64>> = OnceLock::new();
/// The last permission check found notifications turned off; alerts fall back to the sound.
static DENIED: AtomicBool = AtomicBool::new(false);

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

/// Set the Dock icon (bare binary only) and, when bundled, register for notifications. Returns the stream of
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

/// Post a notification for a plugin (`host/notify`): `title` and `body`, clicking it opens
/// thread `thread_key`. Without `banner`, or when notifications are unavailable or not allowed,
/// only the sound plays (if `sound`).
pub fn post(thread_key: Option<u64>, title: &str, body: &str, banner: bool, sound: bool) {
    match center().filter(|_| banner && !DENIED.load(Ordering::Relaxed)) {
        Some(center) => {
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(title));
            content.setBody(&NSString::from_str(body));
            if sound {
                content.setSound(Some(&UNNotificationSound::defaultSound()));
            }
            let thread = thread_key.map_or_else(|| "space".to_string(), |k| format!("{ID_PREFIX}{k}"));
            content.setThreadIdentifier(&NSString::from_str(&thread));
            // Unique per alert, so an older banner for the thread doesn't swallow the new one.
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            let id = NSString::from_str(&format!("{thread}.{nanos}"));
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
            center.addNotificationRequest_withCompletionHandler(&request, None);
        }
        None if sound => play_sound(),
        None => {}
    }
}

/// Whether macOS lets Space post notifications. The answer arrives on a background thread
/// and is sent through the returned channel.
pub fn permission() -> oneshot::Receiver<NotificationPermission> {
    let (tx, rx) = oneshot::channel();
    let Some(center) = center() else {
        let _ = tx.send(NotificationPermission::Unbundled);
        return rx;
    };
    let tx = Mutex::new(Some(tx));
    let done = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        // SAFETY: the center passes a valid settings object for the duration of the call.
        let status = unsafe { settings.as_ref() }.authorizationStatus();
        let permission = if status == UNAuthorizationStatus::Denied {
            NotificationPermission::Denied
        } else if status == UNAuthorizationStatus::NotDetermined {
            NotificationPermission::NotAsked
        } else {
            NotificationPermission::Allowed // authorized, provisional, ephemeral
        };
        DENIED.store(permission == NotificationPermission::Denied, Ordering::Relaxed);
        if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
            let _ = tx.send(permission);
        }
    });
    center.getNotificationSettingsWithCompletionHandler(&done);
    rx
}

/// Ask macOS for permission (shows the system prompt the first time). Resolves once the user
/// has answered, or at once when the answer is already known.
pub fn request_permission() -> oneshot::Receiver<()> {
    let (tx, rx) = oneshot::channel();
    let Some(center) = center() else {
        let _ = tx.send(());
        return rx;
    };
    let tx = Mutex::new(Some(tx));
    let done = RcBlock::new(move |_granted: Bool, _err: *mut NSError| {
        if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
            let _ = tx.send(());
        }
    });
    let options = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
    center.requestAuthorizationWithOptions_completionHandler(options, &done);
    rx
}

/// Open System Settings on Space's notification settings.
pub fn open_system_settings() {
    // On a thread so the `open` child is reaped.
    std::thread::spawn(|| {
        match std::process::Command::new("open").arg(SYSTEM_SETTINGS_URL).status() {
            Ok(status) if status.success() => {}
            Ok(status) => tracing::warn!("opening System Settings failed: `open` exited with {status}"),
            Err(e) => tracing::warn!("opening System Settings failed: {e}"),
        }
    });
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

/// A bare binary has no icon of its own, so set one. A bundle uses its compiled AppIcon, which
/// macOS masks and renders itself; overriding it at runtime would bring back the grey plate.
fn set_dock_icon() {
    if NSBundle::mainBundle().bundleIdentifier().is_some() {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(ICON_PNG);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    }
}
