//! JSON-RPC 2.0 envelope handling for MCP.
//!
//! Kept transport-agnostic on purpose: the dispatcher takes a request value and
//! returns a response value, so it can sit behind HTTP today and anything else
//! later without the protocol logic moving.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Versions of the MCP spec this server knows how to speak.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
pub const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub jsonrpc: String,
    /// Absent for notifications, which take no reply.
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// The subset of JSON-RPC error codes this server can actually produce.
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// Negotiates a protocol version.
///
/// If the client asks for one we speak, we echo it back. Otherwise we answer
/// with our preferred version and let the client decide whether to continue,
/// which is what the spec asks for.
pub fn negotiate_version(requested: Option<&str>) -> &'static str {
    match requested {
        Some(v) => SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .find(|s| **s == v)
            .copied()
            .unwrap_or(DEFAULT_PROTOCOL_VERSION),
        None => DEFAULT_PROTOCOL_VERSION,
    }
}

/// A tool result. MCP wants content blocks, and `isError` for failures the
/// model should see and reason about rather than a transport-level error.
pub fn tool_text(text: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}
