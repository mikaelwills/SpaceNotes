use anyhow::{Context, Result};
use std::sync::Arc;

mod bindings;
mod http;
mod matcher;
mod mcp;
mod spacetime_client;
mod tools;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    tracing::info!("Starting Spacenotes MCP server...");

    // Connect to SpacetimeDB (use env var or default to localhost for all-in-one container)
    let spacetime_host = std::env::var("SPACETIME_HOST")
        .unwrap_or_else(|_| "http://127.0.0.1:3000".to_string());
    let spacetime_db = std::env::var("SPACETIME_DB")
        .unwrap_or_else(|_| "spacenotes".to_string());
    let files_host = std::env::var("SPACENOTES_FILES_HOST").context(
        "SPACENOTES_FILES_HOST is not set. upload_file/download_file hand this URL to an \
         external caller, so there is no safe default - set it to the address the vault's \
         file server is reachable on (e.g. http://100.84.184.121:5051).",
    )?;

    tracing::info!("Connecting to SpacetimeDB at {}/{}", spacetime_host, spacetime_db);
    tracing::info!("Files server reachable at {}", files_host);

    let client = spacetime_client::SpacetimeClient::connect(
        &spacetime_host,
        &spacetime_db,
        &files_host,
    )?;

    let client = Arc::new(client);

    // Start HTTP server with SpacetimeDB client
    http::run_server(client, 5052).await?;

    Ok(())
}
