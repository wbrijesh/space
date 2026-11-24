//! One Space at a time: a Space started while another (same bundle id) runs exits instead, so
//! two never share the settings and saved threads.
//!
//! It doesn't bring the other one forward. Opening Space yourself (Dock, Finder, Spotlight,
//! `open`) while it runs never gets here: macOS brings the running one forward without starting
//! a second. So a second start is from a script or terminal, and making an app active from
//! there would pull you away from what you're doing (see the launch in app.rs).

use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSBundle;

/// The pid of another running Space with this bundle id, if any (this one should exit). An
/// unbundled build (no bundle id) never finds one.
pub fn running_elsewhere() -> Option<i32> {
    let id = NSBundle::mainBundle().bundleIdentifier()?;
    let me = std::process::id() as i32;
    let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(&id);
    running.iter().find(|app| app.processIdentifier() != me && !app.isTerminated()).map(|app| app.processIdentifier())
}
