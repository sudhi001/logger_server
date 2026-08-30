//! The tools an agent can call, and their JSON Schemas.
//!
//! Descriptions here are read by the model, so they say *when* to reach for a
//! tool rather than only what it does — that is what makes an agent pick the
//! right one instead of dumping the last thousand lines and guessing.

use serde_json::{json, Value};

/// What a caller is allowed to do. Set with `LOGGER_MCP_MODE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Query only. Nothing an agent does can change state.
    Read,
    /// Query, plus writing log lines.
    Write,
    /// Everything, including creating and revoking device tokens.
    Admin,
}

impl Access {
    pub fn from_env(raw: Option<&str>) -> Self {
        match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("read") => Access::Read,
            Some("write") => Access::Write,
            // Admin is the default because that is what this deployment asked
            // for; narrow it with LOGGER_MCP_MODE=read where agents are less
            // trusted.
            _ => Access::Admin,
        }
    }

    fn allows(self, tool: &str) -> bool {
        match tool {
            "write_log" => matches!(self, Access::Write | Access::Admin),
            "create_device" | "revoke_device" => matches!(self, Access::Admin),
            _ => true,
        }
    }
}

pub fn is_allowed(access: Access, tool: &str) -> bool {
    access.allows(tool)
}

/// Shared parameter fragments, so the filter vocabulary is identical everywhere.
fn level_prop() -> Value {
    json!({
        "type": "integer", "minimum": 0, "maximum": 4,
        "description": "Minimum severity to include: 0 trace, 1 debug, 2 info, 3 warn, 4 error."
    })
}
fn time_props() -> (Value, Value) {
    (
        json!({ "type": "integer", "description": "Only logs at or after this Unix timestamp in MILLISECONDS." }),
        json!({ "type": "integer", "description": "Only logs at or before this Unix timestamp in MILLISECONDS." }),
    )
}

pub fn list(access: Access) -> Vec<Value> {
    let (since, until) = time_props();

    let mut tools = vec![
        json!({
            "name": "search_logs",
            "description":
                "Search log messages by text, with optional filters. This is the main tool — \
                 prefer it over reading recent logs whenever you know anything about what you \
                 are looking for (an error string, a transaction id, a user id, a tag). Text \
                 matching is full-text over the whole history, not just recent lines. Multiple \
                 words are ANDed, and the final word matches as a prefix. Returns newest first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Free text to look for, e.g. \"NullPointerException\" or a transaction id. Omit to filter without text matching." },
                    "min_level": level_prop(),
                    "device_id": { "type": "integer", "description": "Restrict to one device. Get ids from list_devices." },
                    "name": { "type": "string", "description": "Exact tag match, including trailing spaces, e.g. \"[Net] \"." },
                    "since": since.clone(),
                    "until": until.clone(),
                    "before_id": { "type": "integer", "description": "Pagination cursor: only logs with an id below this. Pass the smallest id from the previous page." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 }
                }
            }
        }),
        json!({
            "name": "get_log_context",
            "description":
                "Return the log lines immediately before and after one specific log, in the \
                 order they happened. Use this after search_logs finds something interesting — \
                 a crash on its own rarely explains itself, and the lines leading up to it \
                 usually do.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "log_id": { "type": "integer", "description": "The id of the log to centre on." },
                    "before": { "type": "integer", "minimum": 0, "maximum": 200, "default": 20 },
                    "after": { "type": "integer", "minimum": 0, "maximum": 200, "default": 20 }
                },
                "required": ["log_id"]
            }
        }),
        json!({
            "name": "get_log_stats",
            "description":
                "Aggregate counts over a time window: totals by severity, by device and by tag, \
                 plus the timestamp range actually covered. Use this FIRST when asked a broad \
                 question like 'what is going wrong' or 'is this affecting everyone' — it \
                 summarises thousands of lines without reading them.",
            "inputSchema": {
                "type": "object",
                "properties": { "since": since, "until": until }
            }
        }),
        json!({
            "name": "get_recent_logs",
            "description":
                "The newest log lines, optionally filtered. Use when you have no search term — \
                 for example to see what is happening right now. If you know what you are \
                 looking for, search_logs is better.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 },
                    "min_level": level_prop(),
                    "device_id": { "type": "integer" }
                }
            }
        }),
        json!({
            "name": "list_devices",
            "description":
                "List registered devices: id, name, platform, when created and when last seen. \
                 Use it to map a device id to a human name, or to find which builds are still \
                 reporting.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ];

    if access.allows("write_log") {
        tools.push(json!({
            "name": "write_log",
            "description":
                "Write a log line, attributed to a device you name. Use sparingly — for \
                 annotating an investigation, not for chatter. Anything written here appears in \
                 the same stream humans are reading.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device_token": { "type": "string", "description": "A device token (lgrd_...) to write as." },
                    "message": { "type": "string" },
                    "name": { "type": "string", "description": "Tag, e.g. \"[agent] \"." },
                    "level": level_prop()
                },
                "required": ["device_token", "message"]
            }
        }));
    }

    if access.allows("create_device") {
        tools.push(json!({
            "name": "create_device",
            "description":
                "Register a new device and return its token. The token is shown once and cannot \
                 be retrieved later. This creates a credential that can write logs — only do it \
                 when the human explicitly asked for a new device.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Human-readable, e.g. \"Pixel 8 - QA\"." },
                    "platform": { "type": "string" }
                },
                "required": ["name"]
            }
        }));
        tools.push(json!({
            "name": "revoke_device",
            "description":
                "Permanently revoke a device's token. It stops working immediately and CANNOT be \
                 restored — the device must be registered again. Destructive: only call it when \
                 the human has clearly asked for this specific device to be revoked.",
            "inputSchema": {
                "type": "object",
                "properties": { "device_id": { "type": "integer" } },
                "required": ["device_id"]
            }
        }));
    }

    tools
}
