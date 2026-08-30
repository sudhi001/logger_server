//! HTTP transport for the MCP server.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use crate::state::AppState;

/// `POST /mcp` — a JSON-RPC request (or batch), authenticated as a viewer.
pub async fn handle(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    if !state.cfg.mcp_enabled {
        return (StatusCode::NOT_FOUND, "mcp is disabled").into_response();
    }
    match crate::mcp::dispatch_value(&state, body).await {
        Some(v) => (StatusCode::OK, Json(v)).into_response(),
        // A notification gets no body, which is what JSON-RPC expects.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// `GET /mcp` — a plain description, so a human who opens the URL in a browser
/// learns what it is instead of seeing a method-not-allowed error.
pub async fn describe(State(state): State<Arc<AppState>>) -> Response {
    let body = serde_json::json!({
        "name": "logger_server",
        "version": env!("CARGO_PKG_VERSION"),
        "transport": "http",
        "protocol": "Model Context Protocol (JSON-RPC 2.0 over POST)",
        "protocolVersions": crate::mcp::protocol::SUPPORTED_PROTOCOL_VERSIONS,
        "access": format!("{:?}", state.cfg.mcp_access).to_lowercase(),
        "tools": crate::mcp::tools::list(state.cfg.mcp_access)
            .iter()
            .filter_map(|t| t.get("name").cloned())
            .collect::<Vec<_>>(),
        "hint": "POST JSON-RPC here with an admin credential to use this endpoint.",
    });
    (StatusCode::OK, Json(body)).into_response()
}
