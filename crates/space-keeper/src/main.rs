//! `space-keeper --socket <path> [--server-log <file> --server <program> <args>…]`: hold Space's
//! agents, and keep the server running, until a server says quit.

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_ansi(false).init();
    let mut args = std::env::args().skip(1);
    let (mut socket, mut log, mut server) = (None, None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = args.next().map(PathBuf::from),
            "--server-log" => log = args.next().map(PathBuf::from),
            "--server" => {
                let program = args.next().map(PathBuf::from).ok_or_else(|| anyhow::anyhow!("--server needs a program"))?;
                server = Some(space_keeper::ServerCommand { program, args: args.by_ref().collect() });
            }
            other => anyhow::bail!("unknown argument `{other}`"),
        }
    }
    let socket = socket.ok_or_else(|| anyhow::anyhow!("usage: space-keeper --socket <path> [--server-log <file> --server <program> <args>…]"))?;
    let log = log.unwrap_or_else(|| socket.with_file_name("server.log"));
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(space_keeper::daemon::run(&socket, server.map(|s| (s, log))))
}
