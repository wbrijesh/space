//! Which build this is. A release is its version ("0.1.6"). A local build carries `build.txt`
//! beside its executable, written by `scripts/dev-bundle.sh` (and copied with SpaceTest): the
//! last release plus what came after, from `git describe` ("0.1.6 +2 · a95ab81 · edited"), and
//! when it was built. `scripts/dev-apps.sh status` reads the same file.

use std::sync::OnceLock;

pub struct BuildInfo {
    /// "0.1.6", or "0.1.6 +2 · a95ab81 · edited" for a local build.
    pub version: String,
    /// When a local build was made ("24 Nov 2026, 00:41"); `None` for a release.
    pub built: Option<String>,
}

pub fn get() -> &'static BuildInfo {
    static INFO: OnceLock<BuildInfo> = OnceLock::new();
    INFO.get_or_init(|| {
        let file = std::env::current_exe().ok().and_then(|exe| std::fs::read_to_string(exe.with_file_name("build.txt")).ok());
        parse(file.as_deref())
    })
}

fn parse(file: Option<&str>) -> BuildInfo {
    let field = |key: &str| {
        file.and_then(|f| f.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=').map(str::trim).map(String::from)))
            .filter(|v| !v.is_empty())
    };
    BuildInfo { version: field("version").unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()), built: field("built") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_without_a_file_and_local_with_one() {
        let release = parse(None);
        assert_eq!(release.version, env!("CARGO_PKG_VERSION"));
        assert!(release.built.is_none());
        let local = parse(Some("version=0.1.6 +2 · a95ab81 · edited\nbuilt=24 Nov 2026, 00:41\nsource=x\n"));
        assert_eq!(local.version, "0.1.6 +2 · a95ab81 · edited");
        assert_eq!(local.built.as_deref(), Some("24 Nov 2026, 00:41"));
    }
}
