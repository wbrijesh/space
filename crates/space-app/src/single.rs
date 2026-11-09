//! One Space at a time: a Space started while another (same bundle id) runs brings that one
//! forward instead, so two never share the settings and saved threads.

use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
use objc2_foundation::NSBundle;

/// Bring forward another running Space; `true` if there was one (this one should exit). An
/// unbundled build (no bundle id) never finds one.
pub fn activate_running() -> bool {
    let Some(id) = NSBundle::mainBundle().bundleIdentifier() else { return false };
    let me = std::process::id() as i32;
    let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(&id);
    let Some(other) = running.iter().find(|app| app.processIdentifier() != me && !app.isTerminated()) else { return false };
    #[allow(deprecated)]
    other.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
    tracing::info!("Space is already running (pid {}): brought it forward", other.processIdentifier());
    true
}
