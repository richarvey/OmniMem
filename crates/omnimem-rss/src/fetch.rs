//! Fetching feeds and article pages over HTTP.

use std::io::Read;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use tracing::warn;

/// 6.x fetched pages as a desktop browser, because many sites serve teaser
/// pages or nothing at all to anything else.
pub const PAGE_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

const FEED_USER_AGENT: &str = concat!("omnimem/", env!("CARGO_PKG_VERSION"), " (RSS reader)");

/// feedparser's Accept header.
const FEED_ACCEPT: &str = "application/atom+xml,application/rdf+xml,application/rss+xml,\
application/x-netcdf,application/xml;q=0.9,text/xml;q=0.2,*/*;q=0.1";

const TIMEOUT: Duration = Duration::from_secs(30);

static SCRIPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<script[^>]*>.*?</script>").expect("valid"));
static STYLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<style[^>]*>.*?</style>").expect("valid"));
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").expect("valid"));
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid"));

/// Plain text from a page: script and style blocks dropped, then every tag.
pub fn page_text(html: &str) -> String {
    let text = SCRIPT.replace_all(html, "");
    let text = STYLE.replace_all(&text, "");
    let text = TAG.replace_all(&text, " ");
    WHITESPACE.replace_all(&text, " ").trim().to_owned()
}

pub struct Fetcher {
    client: reqwest::blocking::Client,
    max_bytes: usize,
}

impl Fetcher {
    pub fn new(max_bytes: usize) -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client, max_bytes })
    }

    /// A body read up to the byte cap: (bytes, whether it was cut short).
    fn get_capped(&self, url: &str, headers: &[(&str, &str)]) -> Result<(Vec<u8>, bool), String> {
        let mut request = self.client.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().map_err(|e| e.to_string())?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("HTTP {status}"));
        }
        let mut raw = Vec::new();
        response
            .take(self.max_bytes as u64 + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        let truncated = raw.len() > self.max_bytes;
        raw.truncate(self.max_bytes);
        Ok((raw, truncated))
    }

    pub fn fetch_feed(&self, url: &str) -> Result<feed_rs::model::Feed, String> {
        let (raw, truncated) = self.get_capped(
            url,
            &[("User-Agent", FEED_USER_AGENT), ("Accept", FEED_ACCEPT)],
        )?;
        if truncated {
            warn!(
                url,
                bytes = self.max_bytes,
                "feed exceeded the byte cap, truncating"
            );
        }
        feed_rs::parser::parse(&raw[..]).map_err(|e| e.to_string())
    }

    /// An article page as plain text, or `None` for a non-HTTP URL or any
    /// failure.
    pub fn fetch_page_content(&self, url: &str) -> Option<String> {
        if !reqwest::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https")) {
            warn!(url, "refusing to fetch non-HTTP URL");
            return None;
        }
        let headers = [
            ("User-Agent", PAGE_USER_AGENT),
            ("Accept", "text/html,application/xhtml+xml"),
            ("Accept-Language", "en-GB,en;q=0.9"),
        ];
        match self.get_capped(url, &headers) {
            Err(e) => {
                warn!(url, error = %e, "failed to fetch page content");
                None
            }
            Ok((raw, truncated)) => {
                if truncated {
                    warn!(
                        url,
                        bytes = self.max_bytes,
                        "page exceeded the byte cap, truncating"
                    );
                }
                Some(page_text(&String::from_utf8_lossy(&raw)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_text_drops_scripts_styles_and_tags() {
        let html = "<html><style>p{}</style><script src=x>var a = '<b>';</script>\n<p>Hello <b>world</b></p></html>";
        assert_eq!(page_text(html), "Hello world");
    }
}
