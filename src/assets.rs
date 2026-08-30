//! Static assets compiled into the binary.
//!
//! Embedded rather than read from disk so the container can be `FROM scratch`
//! with no filesystem at all. Nothing here is loaded from a CDN either, so the
//! dashboard works on an isolated network.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

pub struct Asset {
    pub body: &'static [u8],
    pub content_type: &'static str,
    pub etag: &'static str,
}

const HTML: &str = "text/html; charset=utf-8";
const JS: &str = "application/javascript; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";

macro_rules! asset {
    ($path:literal, $ctype:expr, $tag:literal) => {
        Some(Asset {
            body: include_bytes!(concat!("../static/", $path)),
            content_type: $ctype,
            etag: concat!("\"", env!("CARGO_PKG_VERSION"), "-", $tag, "\""),
        })
    };
}

pub fn lookup(path: &str) -> Option<Asset> {
    match path {
        "/" | "/index.html" => asset!("index.html", HTML, "index"),
        "/login.html" => asset!("login.html", HTML, "login"),
        "/devices.html" => asset!("devices.html", HTML, "devices"),
        "/alerts.html" => asset!("alerts.html", HTML, "alerts"),
        "/app.js" => asset!("app.js", JS, "appjs"),
        "/login.js" => asset!("login.js", JS, "loginjs"),
        "/devices.js" => asset!("devices.js", JS, "devicesjs"),
        "/alerts.js" => asset!("alerts.js", JS, "alertsjs"),
        "/app.css" => asset!("app.css", CSS, "appcss"),
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
