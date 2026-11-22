//! `/static/*`: the stylesheet, htmx and the self-hosted fonts, embedded.

use std::collections::HashMap;
use std::sync::LazyLock;

use axum::extract::{Path, Query};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::embedded::ASSETS;

/// A short hash of every embedded file. Pages link the stylesheet, scripts
/// and images with `?v=` this, so a browser may keep them for a year and
/// still picks up a new build the moment any file changes (a version number
/// wouldn't do: development builds share one).
pub(crate) static ASSET_VERSION: LazyLock<String> = LazyLock::new(|| {
    let mut hash = Sha256::new();
    for (name, bytes) in ASSETS {
        hash.update(name.as_bytes());
        hash.update([0]);
        hash.update(bytes);
    }
    hex(&hash.finalize()[..6])
});

/// Each file's ETag, from its own bytes.
static ETAGS: LazyLock<HashMap<&'static str, String>> = LazyLock::new(|| {
    ASSETS
        .iter()
        .map(|(name, bytes)| (*name, format!("\"{}\"", hex(&Sha256::digest(bytes)[..8]))))
        .collect()
});

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("woff2") => "font/woff2",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// A file, cached: for a year when asked for under this build's
/// [`ASSET_VERSION`], since that address changes with the file, and
/// otherwise revalidated on every use (the fonts, which the stylesheet names
/// without a version). Either way an unchanged file answers a revalidation
/// with a 304 and no body. Without this, every page fetched about 290 KB of
/// stylesheet, scripts, fonts and images again.
pub(crate) async fn serve(
    Path(path): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some((name, bytes)) = ASSETS.iter().find(|(name, _)| *name == path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let cache = if query.get("v") == Some(&*ASSET_VERSION) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let etag = &ETAGS[name];
    let unchanged = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|tag| tag.trim() == etag || tag.trim() == "*")
        });
    let mut response = if unchanged {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        ([(header::CONTENT_TYPE, content_type(name))], *bytes).into_response()
    };
    let out = response.headers_mut();
    out.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Ok(value) = HeaderValue::from_str(etag) {
        out.insert(header::ETAG, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(path: &str, v: Option<&str>, if_none_match: Option<&str>) -> Response {
        let query = v
            .map(|v| HashMap::from([("v".to_owned(), v.to_owned())]))
            .unwrap_or_default();
        let mut headers = HeaderMap::new();
        if let Some(tag) = if_none_match {
            headers.insert(header::IF_NONE_MATCH, HeaderValue::from_str(tag).unwrap());
        }
        serve(Path(path.to_owned()), Query(query), headers).await
    }

    #[tokio::test]
    async fn versioned_files_cache_for_a_year_and_others_revalidate() {
        let versioned = get("style.css", Some(&ASSET_VERSION), None).await;
        assert_eq!(versioned.status(), StatusCode::OK);
        assert_eq!(
            versioned.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        let stale = get("style.css", Some("not-this-build"), None).await;
        assert_eq!(stale.headers()[header::CACHE_CONTROL], "no-cache");

        let etag = versioned.headers()[header::ETAG]
            .to_str()
            .unwrap()
            .to_owned();
        let again = get("style.css", None, Some(&etag)).await;
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(again.headers()[header::ETAG], etag.as_str());
        let changed = get("style.css", None, Some("\"something-else\"")).await;
        assert_eq!(changed.status(), StatusCode::OK);
        assert_eq!(
            get("nope.css", None, None).await.status(),
            StatusCode::NOT_FOUND
        );
    }
}
