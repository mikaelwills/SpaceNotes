use anyhow::Result;
use std::sync::Arc;

mod bindings;
mod http;
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

    tracing::info!("Connecting to SpacetimeDB at {}/{}", spacetime_host, spacetime_db);

    let client = spacetime_client::SpacetimeClient::connect(
        &spacetime_host,
        &spacetime_db
    )?;

    let client = Arc::new(client);

    // Start HTTP server with SpacetimeDB client
    http::run_server(client, 5052).await?;

    Ok(())
}
