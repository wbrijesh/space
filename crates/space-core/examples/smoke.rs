//! Headless end-to-end check.
//!
//! ```sh
//! cargo run -p space-core --example smoke -- [--plugin ID] [--cwd DIR] [--allow] "Reply with exactly: pong"
//! ```

use std::time::Duration;

use space_core::smoke::{self, SmokeOptions};
use space_core::{Host, HostOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut plugin_id = None;
    let mut cwd = std::env::current_dir()?;
    let mut allow = false;
    let mut prompt = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--plugin" => plugin_id = args.next(),
            "--cwd" => cwd = args.next().expect("--cwd DIR").into(),
            "--allow" => allow = true,
            _ => prompt = Some(arg),
        }
    }
    let prompt = prompt.unwrap_or_else(|| "Reply with exactly: pong".into());
    let (host, events) = Host::new(tokio::runtime::Handle::current(), HostOptions::for_project(&cwd));
    let opts = SmokeOptions { plugin_id, cwd, prompt, allow_permissions: allow, timeout: Duration::from_secs(120) };
    let state = smoke::run(&host, events, opts).await?;
    println!("transcript items: {}", state.items.len());
    Ok(())
}
