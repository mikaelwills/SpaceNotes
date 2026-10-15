#[path = "../../../spacenotes-mcp/src/bindings/mod.rs"]
mod bindings;
mod bridge;
mod cleanup;
mod launch;
mod log;
mod server;
mod update;

use std::sync::Arc;

const BUILD_SHA: &str = match option_env!("SPACE_CHANNEL_BUILD_SHA") {
    Some(sha) => sha,
    None => "dev",
};

pub const DEFAULT_STDB_URI: &str = "ws://100.84.184.121:5050";
pub const DEFAULT_STDB_DB: &str = "spacenotes";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(subcommand) = args.first() else {
        eprintln!("usage: space-channel <version|launch|bridge|cleanup> [args...]");
        std::process::exit(2);
    };
    match subcommand.as_str() {
        "version" => println!("{BUILD_SHA}"),
        "launch" => launch::run(&args[1..]),
        "bridge" => run_bridge(&args[1..]),
        "cleanup" => cleanup::run(),
        other => {
            eprintln!("unknown subcommand: {other}");
            std::process::exit(2);
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let idx = args.iter().position(|a| a == name)?;
    args.get(idx + 1).cloned()
}

fn run_bridge(args: &[String]) {
    let Some(socket) = flag(args, "--socket") else {
        eprintln!("usage: space-channel bridge --socket <path> [--agent <name>]");
        std::process::exit(2);
    };
    let agent = flag(args, "--agent")
        .or_else(|| std::env::var("SPACE_CHANNEL_AGENT").ok())
        .unwrap_or_else(|| format!("agent-{}", std::process::id()));
    let stdb_uri = std::env::var("SPACE_CHANNEL_STDB_URI").unwrap_or_else(|_| DEFAULT_STDB_URI.into());
    let stdb_db = std::env::var("SPACE_CHANNEL_STDB_DB").unwrap_or_else(|_| DEFAULT_STDB_DB.into());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(async move {
        let bridge = Arc::new(bridge::Bridge::new(&agent, &stdb_uri, &stdb_db, BUILD_SHA));
        bridge.clone().start();
        if let Err(err) = server::serve(bridge.clone(), &socket).await {
            bridge.log(&format!("socket server failed: {err}"));
        }
        bridge.shutdown().await;
        let _ = std::fs::remove_file(&socket);
    });
}
