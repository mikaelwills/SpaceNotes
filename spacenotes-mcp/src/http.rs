use anyhow::Result;
use axum::{extract::State, routing::post, Json, Router};
use serde_json::json;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::{mcp, spacetime_client::SpacetimeClient, tools};

pub async fn run_server(client: Arc<SpacetimeClient>, port: u16) -> Result<()> {
    let app = Router::new()
        .route("/", post(mcp_handler))
        .route("/mcp", post(mcp_handler))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(client);

    let addr = format!("[::]:{}", port);
    tracing::info!("Spacenotes MCP server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn mcp_handler(
    State(client): State<Arc<SpacetimeClient>>,
    Json(request): Json<mcp::Request>,
) -> Json<serde_json::Value> {
    let response = handle_request(&client, request).await;
    Json(response)
}

async fn handle_request(
    client: &SpacetimeClient,
    request: mcp::Request,
) -> serde_json::Value {
    match request.method.as_str() {
        "initialize" => {
            json!({
                "jsonrpc": "2.0",
                "id": request.id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "serverInfo": {
                        "name": "spacenotes-mcp",
                        "version": "0.1.0"
                    }
                }
            })
        }
        "tools/list" => {
            json!({
                "jsonrpc": "2.0",
                "id": request.id,
                "result": {"tools": tools::get_tools()}
            })
        }
        "tools/call" => {
            let params: tools::ToolCallParams =
                serde_json::from_value(request.params).unwrap();

            match tools::execute_tool(client, params).await {
                Ok(result) => json!({"jsonrpc": "2.0", "id": request.id, "result": result}),
                Err(err) => json!({"jsonrpc": "2.0", "id": request.id, "error": {"code": -32603, "message": err}})
            }
        }
        _ => json!({"jsonrpc": "2.0", "id": request.id, "error": {"code": -32601, "message": "Method not found"}})
    }
}
