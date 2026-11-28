//! `space-keeper --socket <path>`: hold Space's agents until a server says quit.

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_ansi(false).init();
    let mut args = std::env::args().skip(1);
    let mut socket = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = args.next().map(PathBuf::from),
            other => anyhow::bail!("unknown argument `{other}`"),
        }
    }
    let socket = socket.ok_or_else(|| anyhow::anyhow!("usage: space-keeper --socket <path>"))?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(space_keeper::daemon::run(&socket))
}
