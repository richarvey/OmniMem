//! `/static/*`: the stylesheet, htmx and the self-hosted fonts, embedded.

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::embedded::ASSETS;

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("woff2") => "font/woff2",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

pub(crate) async fn serve(Path(path): Path<String>) -> Response {
    match ASSETS.iter().find(|(name, _)| *name == path) {
        Some((name, bytes)) => {
            ([(header::CONTENT_TYPE, content_type(name))], *bytes).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
