//! Static assets compiled into the binary.
//!
//! Embedded rather than read from disk so the container can be `FROM scratch`
//! with no filesystem at all.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

pub struct Asset {
    pub body: &'static [u8],
    pub content_type: &'static str,
    pub etag: &'static str,
}

const INDEX_HTML: &[u8] = include_bytes!("../static/index.html");
const LOGS_JS: &[u8] = include_bytes!("../static/logs.js");
const LOG_CSS: &[u8] = include_bytes!("../static/log.css");

pub fn lookup(path: &str) -> Option<Asset> {
    match path {
        "/" | "/index.html" => Some(Asset {
            body: INDEX_HTML,
            content_type: "text/html; charset=utf-8",
            etag: concat!("\"", env!("CARGO_PKG_VERSION"), "-index\""),
        }),
        "/logs.js" => Some(Asset {
            body: LOGS_JS,
            content_type: "application/javascript; charset=utf-8",
            etag: concat!("\"", env!("CARGO_PKG_VERSION"), "-js\""),
        }),
        "/log.css" => Some(Asset {
            body: LOG_CSS,
            content_type: "text/css; charset=utf-8",
            etag: concat!("\"", env!("CARGO_PKG_VERSION"), "-css\""),
        }),
        _ => None,
    }
}

impl IntoResponse for Asset {
    fn into_response(self) -> Response {
        (
            StatusCode::OK,
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(self.content_type),
                ),
                (header::ETAG, HeaderValue::from_static(self.etag)),
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=300"),
                ),
            ],
            self.body,
        )
            .into_response()
    }
}
