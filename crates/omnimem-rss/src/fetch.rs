//! Fetching feeds and article pages over HTTP.
//!
//! Every URL fetched here came from outside: a feed URL from `feeds.yml`, an
//! entry's own link from the feed body, or a redirect from either server.
//! So each one is checked before a request is made, and again on every
//! redirect hop: only `http` and `https`, and only hosts that resolve to
//! public addresses. Without that, a feed could point the server at the
//! panel, the MCP endpoint or anything else on the host's networks.
//!
//! The host is resolved here and again by reqwest for the request itself,
//! so a name that changes its answer between the two (DNS rebinding) is not
//! caught. That is out of scope; the check is against a feed that names a
//! private address outright.

use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::{Captures, Regex};
use tracing::warn;

/// 6.x fetched pages as a desktop browser, because many sites serve teaser
/// pages or nothing at all to anything else.
pub const PAGE_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

const FEED_USER_AGENT: &str = concat!("omnimem/", env!("CARGO_PKG_VERSION"), " (RSS reader)");

/// feedparser's Accept header.
const FEED_ACCEPT: &str = "application/atom+xml,application/rdf+xml,application/rss+xml,\
application/x-netcdf,application/xml;q=0.9,text/xml;q=0.2,*/*;q=0.1";

/// reqwest's timeout re-arms on every read, so on its own it never ends a
/// server that drips a byte at a time. `BUDGET` is the wall-clock limit on a
/// whole fetch, checked between chunks.
const TIMEOUT: Duration = Duration::from_secs(30);
const BUDGET: Duration = Duration::from_mins(1);
const MAX_REDIRECTS: usize = 5;
const CHUNK: usize = 64 * 1024;

static SCRIPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<script[^>]*>.*?</script>").expect("valid"));
static STYLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<style[^>]*>.*?</style>").expect("valid"));
static COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->").expect("valid"));
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").expect("valid"));
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid"));
static ENTITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"&(?:#[xX]([0-9a-fA-F]{1,6})|#([0-9]{1,7})|(amp|lt|gt|quot|apos|nbsp));")
        .expect("valid")
});

/// Plain text from HTML: script, style and comment blocks dropped, every
/// tag replaced with `tag_replacement`, the common entities decoded, and
/// whitespace collapsed. Entities are decoded after the tags go, so
/// `&lt;script&gt;` in a page's text stays text.
pub(crate) fn strip_markup(html: &str, tag_replacement: &str) -> String {
    let text = SCRIPT.replace_all(html, "");
    let text = STYLE.replace_all(&text, "");
    let text = COMMENT.replace_all(&text, "");
    let text = TAG.replace_all(&text, tag_replacement);
    let text = ENTITY.replace_all(&text, |caps: &Captures| decode_entity(caps));
    WHITESPACE.replace_all(&text, " ").trim().to_owned()
}

fn decode_entity(caps: &Captures) -> String {
    let numeric = caps
        .get(1)
        .map(|hex| u32::from_str_radix(hex.as_str(), 16))
        .or_else(|| caps.get(2).map(|dec| dec.as_str().parse::<u32>()));
    if let Some(parsed) = numeric {
        // A code point that isn't a character, or is NUL, is left as written.
        return parsed
            .ok()
            .filter(|n| *n != 0)
            .and_then(char::from_u32)
            .map_or_else(|| caps[0].to_owned(), String::from);
    }
    match &caps[3] {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        _ => &caps[0],
    }
    .to_owned()
}

/// Plain text from a page: script and style blocks dropped, then every tag.
pub fn page_text(html: &str) -> String {
    strip_markup(html, " ")
}

/// True when `host` names only public addresses. A literal address is
/// checked as it is; a name is resolved and every address it resolves to
/// must be public. Loopback, the private ranges, link-local, unique-local,
/// unspecified, multicast, broadcast and IPv4-mapped IPv6 addresses are all
/// refused, as is `localhost` and a name that does not resolve at all.
/// An error with its causes, so "error following redirect" says what the
/// redirect policy refused.
fn describe(error: &reqwest::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        parts.push(cause.to_string());
        source = cause.source();
    }
    parts.join(": ")
}

pub fn is_public_host(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return false;
    }
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        return false;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_public_ip(ip);
    }
    match (host, 0u16).to_socket_addrs() {
        Ok(addrs) => {
            let mut resolved = false;
            for addr in addrs {
                resolved = true;
                if !is_public_ip(addr.ip()) {
                    return false;
                }
            }
            resolved
        }
        Err(_) => false,
    }
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            // 100.64/10 is carrier NAT and 0/8 "this network"; neither is a
            // place a feed should send us.
            let shared = o[0] == 100 && (o[1] & 0xc0) == 64;
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_documentation()
                || shared
                || o[0] == 0)
        }
        IpAddr::V6(v6) => {
            // An IPv4 address carried in IPv6 (`::ffff:10.0.0.1`) would slip
            // past the IPv4 checks, so every embedded IPv4 form is refused.
            if v6.to_ipv4().is_some() {
                return false;
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local())
        }
    }
}

/// The scheme and host check every fetched URL passes, including each
/// redirect target.
fn check_url(url: &reqwest::Url, allow_private_hosts: bool) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("refusing non-HTTP URL scheme {:?}", url.scheme()));
    }
    let host = url.host_str().ok_or_else(|| "URL has no host".to_owned())?;
    if !allow_private_hosts && !is_public_host(host) {
        return Err(format!(
            "refusing host {host:?}: not a public address, or it does not resolve"
        ));
    }
    Ok(())
}

pub struct Fetcher {
    client: reqwest::blocking::Client,
    max_bytes: usize,
    allow_private_hosts: bool,
    budget: Duration,
}

impl Fetcher {
    /// A fetcher that refuses private and loopback hosts unless the
    /// `RSS_ALLOW_PRIVATE_HOSTS` setting is on, for feeds served on a LAN.
    pub fn new(max_bytes: usize) -> Result<Self, String> {
        let allow = omnimem_core::env::var("RSS_ALLOW_PRIVATE_HOSTS").is_some_and(|v| {
            matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
        });
        if allow {
            warn!("RSS_ALLOW_PRIVATE_HOSTS is on: feeds may fetch private and loopback addresses");
        }
        Self::with_private_hosts(max_bytes, allow)
    }

    /// A fetcher with the private-host policy chosen explicitly. Tests use
    /// this to reach a server on 127.0.0.1 without weakening the check.
    pub fn with_private_hosts(max_bytes: usize, allow_private_hosts: bool) -> Result<Self, String> {
        Self::build(max_bytes, allow_private_hosts, BUDGET)
    }

    fn build(
        max_bytes: usize,
        allow_private_hosts: bool,
        budget: Duration,
    ) -> Result<Self, String> {
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error(format!("more than {MAX_REDIRECTS} redirects"));
            }
            match check_url(attempt.url(), allow_private_hosts) {
                Ok(()) => attempt.follow(),
                Err(e) => attempt.error(format!("redirect refused: {e}")),
            }
        });
        let client = reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .redirect(policy)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            max_bytes,
            allow_private_hosts,
            budget,
        })
    }

    /// A body read up to the byte cap: (bytes, whether it was cut short).
    /// The URL is checked first, and the read gives up once the whole fetch
    /// has run past the budget.
    fn get_capped(&self, url: &str, headers: &[(&str, &str)]) -> Result<(Vec<u8>, bool), String> {
        let parsed = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
        check_url(&parsed, self.allow_private_hosts)?;
        let start = Instant::now();
        let mut request = self.client.get(parsed);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut response = request.send().map_err(|e| describe(&e))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("HTTP {status}"));
        }
        let mut raw = Vec::new();
        let mut buf = vec![0u8; CHUNK.min(self.max_bytes + 1)];
        loop {
            if start.elapsed() > self.budget {
                return Err(format!(
                    "fetch ran past the {} second budget",
                    self.budget.as_secs()
                ));
            }
            let n = response.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            let room = self.max_bytes + 1 - raw.len();
            raw.extend_from_slice(&buf[..n.min(room)]);
            if raw.len() > self.max_bytes {
                break;
            }
        }
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

    /// An article page as plain text, or `None` for a non-HTTP URL, a
    /// private host or any failure.
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
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn page_text_drops_scripts_styles_and_tags() {
        let html = "<html><style>p{}</style><script src=x>var a = '<b>';</script>\n<p>Hello <b>world</b></p></html>";
        assert_eq!(page_text(html), "Hello world");
    }

    #[test]
    fn block_removal_ignores_case_and_drops_comments() {
        let html = "<SCRIPT>alert(1)</Script><STYLE>b{}</style><!-- hidden <b>note</b> -->\
                    <P>shown</P>";
        assert_eq!(page_text(html), "shown");
        assert_eq!(strip_markup("<!-- a\nmulti-line -->x", ""), "x");
    }

    #[test]
    fn entities_decode_after_tags_are_gone() {
        assert_eq!(
            page_text("Tom &amp; Jerry &lt;b&gt;bold&lt;/b&gt; &quot;q&quot; &#39;s&#39;&nbsp;end"),
            "Tom & Jerry <b>bold</b> \"q\" 's' end"
        );
        assert_eq!(page_text("&#x41;&#66;&#x1F600;"), "AB\u{1F600}");
        assert_eq!(page_text("&#0;&#xD800;&bogus;"), "&#0;&#xD800;&bogus;");
        // Decoded markup is text, not a tag to strip.
        assert_eq!(page_text("&lt;script&gt;x"), "<script>x");
    }

    #[test]
    fn private_and_special_hosts_are_not_public() {
        for host in [
            "localhost",
            "LOCALHOST",
            "foo.localhost",
            "127.0.0.1",
            "127.1.2.3",
            "10.0.0.5",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "[::1]",
            "::1",
            "[::]",
            "[fe80::1]",
            "[fc00::1]",
            "[fd12::1]",
            "[ff02::1]",
            "[::ffff:127.0.0.1]",
            "[::ffff:10.0.0.1]",
            "",
        ] {
            assert!(!is_public_host(host), "{host:?} must be refused");
        }
        for host in [
            "1.1.1.1",
            "8.8.8.8",
            "[2606:4700:4700::1111]",
            "203.0.114.1",
        ] {
            assert!(is_public_host(host), "{host:?} must be allowed");
        }
    }

    /// One connection at a time: redirects to a non-HTTP scheme and to
    /// itself, and a drip-feed that sends a byte every 50 ms for as long as
    /// it is read.
    fn serve() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let origin = base.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                if reader.read_line(&mut request).is_err() {
                    continue;
                }
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line.trim().is_empty() => break,
                        Ok(_) => {}
                    }
                }
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                match path.as_str() {
                    "/to-ftp" => {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 302 Found\r\nlocation: ftp://example.org/x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        );
                    }
                    "/loop" => {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 302 Found\r\nlocation: {origin}/loop\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        );
                    }
                    "/drip" => {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\nconnection: close\r\n\r\n"
                        );
                        while stream.write_all(b"a").is_ok() {
                            let _ = stream.flush();
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                    _ => {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\ncontent-length: 5\r\nconnection: close\r\n\r\nhello"
                        );
                    }
                }
            }
        });
        base
    }

    #[test]
    fn loopback_is_refused_unless_allowed() {
        let base = serve();
        let strict = Fetcher::with_private_hosts(1024, false).unwrap();
        let err = strict.get_capped(&format!("{base}/page"), &[]).unwrap_err();
        assert!(err.contains("refusing host"), "{err}");
        assert!(strict.fetch_page_content(&format!("{base}/page")).is_none());
        assert!(strict.fetch_feed(&format!("{base}/feed")).is_err());
        assert!(strict.fetch_page_content("ftp://example.org/x").is_none());

        let lenient = Fetcher::with_private_hosts(1024, true).unwrap();
        assert_eq!(
            lenient.fetch_page_content(&format!("{base}/page")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn redirects_are_rechecked_and_capped() {
        let base = serve();
        // Loopback is allowed so the test server can answer at all; the
        // scheme check and the hop limit still apply to every redirect.
        let lenient = Fetcher::with_private_hosts(1024, true).unwrap();
        let err = lenient
            .get_capped(&format!("{base}/to-ftp"), &[])
            .unwrap_err();
        assert!(err.contains("redirect refused"), "{err}");
        assert!(err.contains("ftp"), "{err}");
        let err = lenient
            .get_capped(&format!("{base}/loop"), &[])
            .unwrap_err();
        assert!(err.contains("redirects"), "{err}");
    }

    #[test]
    fn a_redirect_to_a_private_host_is_refused_even_when_the_first_hop_passed() {
        // A policy built for public hosts only, exercised through the
        // redirect check directly: the closure sees the same URLs reqwest
        // would hand it.
        let url = reqwest::Url::parse("http://127.0.0.1:1/x").unwrap();
        assert!(check_url(&url, false).is_err());
        assert!(check_url(&url, true).is_ok());
        let url = reqwest::Url::parse("http://[::ffff:127.0.0.1]/x").unwrap();
        assert!(check_url(&url, false).is_err());
        let url = reqwest::Url::parse("file:///etc/passwd").unwrap();
        assert!(check_url(&url, true).is_err());
    }

    #[test]
    fn a_drip_fed_body_hits_the_wall_clock_budget() {
        let base = serve();
        let fetcher = Fetcher::build(1024 * 1024, true, Duration::from_millis(400)).unwrap();
        let started = Instant::now();
        let err = fetcher
            .get_capped(&format!("{base}/drip"), &[])
            .unwrap_err();
        assert!(err.contains("budget"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "gave up promptly"
        );
    }
}
