//! Fetching model files from the Hugging Face hub into the shared cache.
//!
//! Files land where `huggingface_hub` would look for them
//! (`models--{owner}--{name}/snapshots/{commit}/{file}`, with `refs/{branch}`
//! naming the commit), so a cache filled by 6.x Python and one filled by this
//! engine are interchangeable. A file the repo doesn't have is recorded under
//! `.no_exist/`, as the Python library does.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::error::EmbedError;

pub(crate) const DEFAULT_ENDPOINT: &str = "https://huggingface.co";

/// The most one model file may be. The default model is under 100 MB; a
/// hub answer that runs past this is not a model this engine can load.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// reqwest's timeout re-arms on every read, so a server that drips bytes
/// would hold the download forever; this is the wall-clock limit on one
/// file, checked between chunks.
const BUDGET: Duration = Duration::from_mins(15);
const CHUNK: usize = 64 * 1024;
/// Redirects followed for one file: the hub's rename hops plus its CDN.
const MAX_REDIRECTS: usize = 10;

pub(crate) struct Downloader {
    endpoint: String,
    token: Option<String>,
    cache: PathBuf,
    client: reqwest::blocking::Client,
    /// The same client without redirects, for files: `fetch` follows them
    /// itself so it can tell the hub's answers from the CDN's.
    direct: reqwest::blocking::Client,
    max_file_bytes: u64,
    budget: Duration,
}

pub(crate) enum Fetched {
    Present(PathBuf),
    Absent,
}

impl Downloader {
    pub(crate) fn new(
        endpoint: &str,
        token: Option<String>,
        cache: PathBuf,
    ) -> Result<Self, EmbedError> {
        let builder = || {
            reqwest::blocking::Client::builder()
                .user_agent(concat!("omnimem/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(30))
                .timeout(Duration::from_mins(15))
        };
        let client = builder()
            .build()
            .map_err(|e| EmbedError::Download(e.to_string()))?;
        let direct = builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| EmbedError::Download(e.to_string()))?;
        Ok(Self {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            token,
            cache,
            client,
            direct,
            max_file_bytes: MAX_FILE_BYTES,
            budget: BUDGET,
        })
    }

    /// `HF_ENDPOINT` and `HF_TOKEN`, as the Python library reads them.
    pub(crate) fn from_env(cache: PathBuf) -> Result<Self, EmbedError> {
        let var = |name: &str| {
            omnimem_core::env::var(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let endpoint = var("HF_ENDPOINT").unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());
        warn_once_if_insecure(&endpoint);
        Self::new(&endpoint, var("HF_TOKEN"), cache)
    }

    fn repo_dir(&self, repo: &str) -> PathBuf {
        self.cache
            .join(format!("models--{}", repo.replace('/', "--")))
    }

    fn get(&self, url: &str) -> Result<reqwest::blocking::Response, EmbedError> {
        let mut request = self.client.get(url);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        request
            .send()
            .map_err(|e| EmbedError::Download(format!("{url}: {e}")))
    }

    /// GET a file, following redirects by hand, with the sha256 the hub
    /// names for it. Only the hub's own responses are believed: an LFS
    /// file's sha256 is in the `x-linked-etag` of the hub's redirect, and
    /// the CDN it redirects to sends an `etag` of its own, which for Xet
    /// storage is 64 hex characters of content hash and not the sha256 of
    /// the bytes. Following redirects automatically kept only the CDN's
    /// headers, so every LFS download was refused. The token goes only to
    /// the hub, as `huggingface_hub` does.
    fn open_file(
        &self,
        url: &str,
    ) -> Result<(reqwest::blocking::Response, Option<String>), EmbedError> {
        let fail = |at: &dyn std::fmt::Display, e: &dyn std::fmt::Display| {
            EmbedError::Download(format!("{at}: {e}"))
        };
        let hub = reqwest::Url::parse(&self.endpoint).map_err(|e| fail(&self.endpoint, &e))?;
        let mut current = reqwest::Url::parse(url).map_err(|e| fail(&url, &e))?;
        let mut expected = None;
        for _ in 0..=MAX_REDIRECTS {
            let on_hub = current.origin() == hub.origin();
            let mut request = self.direct.get(current.clone());
            if on_hub && let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }
            let response = request.send().map_err(|e| fail(&current, &e))?;
            if on_hub && expected.is_none() {
                expected = expected_sha256(response.headers());
            }
            if !response.status().is_redirection() {
                return Ok((response, expected));
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| fail(&current, &"a redirect with no location"))?;
            current = current.join(location).map_err(|e| fail(&current, &e))?;
        }
        Err(fail(&url, &format!("more than {MAX_REDIRECTS} redirects")))
    }

    /// The commit a revision names. A 40-character commit is used as given;
    /// a branch or tag is asked of the hub and recorded under `refs/`.
    pub(crate) fn resolve_commit(&self, repo: &str, revision: &str) -> Result<String, EmbedError> {
        if is_commit(revision) {
            return Ok(revision.to_owned());
        }
        check_revision(revision)?;
        let url = format!("{}/api/models/{repo}/revision/{revision}", self.endpoint);
        let response = self.get(&url)?;
        let status = response.status();
        if !status.is_success() {
            return Err(EmbedError::Download(format!("{url}: HTTP {status}")));
        }
        let body = response
            .text()
            .map_err(|e| EmbedError::Download(format!("{url}: {e}")))?;
        let sha = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("sha").and_then(Value::as_str).map(str::to_owned))
            .filter(|s| is_commit(s))
            .ok_or_else(|| EmbedError::Download(format!("{url}: no commit sha in the response")))?;
        let refs = self.repo_dir(repo).join("refs");
        fs::create_dir_all(&refs).map_err(|e| io(&refs, e))?;
        let ref_file = refs.join(revision);
        fs::write(&ref_file, &sha).map_err(|e| io(&ref_file, e))?;
        Ok(sha)
    }

    /// Download one file into the snapshot, or record that the repo lacks it.
    /// A file already present is returned without a request.
    pub(crate) fn fetch(
        &self,
        repo: &str,
        commit: &str,
        file: &str,
    ) -> Result<Fetched, EmbedError> {
        let base = self.repo_dir(repo);
        let target = base.join("snapshots").join(commit).join(file);
        if target.is_file() {
            return Ok(Fetched::Present(target));
        }
        if base.join(".no_exist").join(commit).join(file).is_file() {
            return Ok(Fetched::Absent);
        }

        let url = format!("{}/{repo}/resolve/{commit}/{file}", self.endpoint);
        let (mut response, expected) = self.open_file(&url)?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            let marker = base.join(".no_exist").join(commit).join(file);
            if let Some(parent) = marker.parent() {
                fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
            }
            fs::write(&marker, b"").map_err(|e| io(&marker, e))?;
            return Ok(Fetched::Absent);
        }
        if !status.is_success() {
            return Err(EmbedError::Download(format!("{url}: HTTP {status}")));
        }
        if expected.is_none() {
            debug!(
                repo,
                file, "no sha256 etag on the response; not verifying the download"
            );
        }

        let parent = target
            .parent()
            .ok_or_else(|| EmbedError::Download(format!("bad file name {file}")))?;
        fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
        // Written beside the target under a name unique to this process and
        // download, then renamed into place: an interrupted download never
        // leaves a truncated model where the engine looks, and two processes
        // fetching the same file at once don't write into each other's copy.
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let partial = parent.join(format!("{name}.{}.incomplete", unique_suffix()));
        let written = self.copy_body(&url, &mut response, &partial, expected.as_deref());
        let bytes = match written {
            Ok(bytes) => bytes,
            Err(e) => {
                let _ = fs::remove_file(&partial);
                return Err(e);
            }
        };
        fs::rename(&partial, &target).map_err(|e| io(&target, e))?;
        info!(repo, file, bytes, "downloaded model file");
        Ok(Fetched::Present(target))
    }

    /// The body into `partial`, in chunks, giving up past the byte cap or
    /// the wall-clock budget, and refusing a body whose sha256 does not
    /// match the hub's etag when the hub sent one.
    fn copy_body(
        &self,
        url: &str,
        response: &mut reqwest::blocking::Response,
        partial: &Path,
        expected: Option<&str>,
    ) -> Result<u64, EmbedError> {
        let start = Instant::now();
        let mut out = fs::File::create(partial).map_err(|e| io(partial, e))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        let mut bytes: u64 = 0;
        loop {
            if start.elapsed() > self.budget {
                return Err(EmbedError::Download(format!(
                    "{url}: the download ran past the {} second budget",
                    self.budget.as_secs()
                )));
            }
            let n = response
                .read(&mut buf)
                .map_err(|e| EmbedError::Download(format!("{url}: {e}")))?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            if bytes > self.max_file_bytes {
                return Err(EmbedError::Download(format!(
                    "{url}: more than {} bytes",
                    self.max_file_bytes
                )));
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(|e| io(partial, e))?;
        }
        out.flush().map_err(|e| io(partial, e))?;
        drop(out);
        if let Some(expected) = expected {
            let actual = hex(&hasher.finalize());
            if actual != expected {
                return Err(EmbedError::Download(format!(
                    "{url}: sha256 {actual} does not match the hub's {expected}"
                )));
            }
            debug!(url, "download matched the hub's sha256");
        }
        Ok(bytes)
    }
}

/// The hub names a file's sha256 in `x-linked-etag` (for LFS files) or
/// `etag`, quoted and possibly weak. Only a 64-hex value is a sha256; a
/// short etag is a git blob id and can't be checked here.
fn expected_sha256(headers: &reqwest::header::HeaderMap) -> Option<String> {
    ["x-linked-etag", "etag"]
        .iter()
        .filter_map(|name| headers.get(*name))
        .filter_map(|v| v.to_str().ok())
        .map(|v| {
            v.trim()
                .trim_start_matches("W/")
                .trim_matches('"')
                .to_ascii_lowercase()
        })
        .find(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Lowercase hex, as the hub writes a sha256 etag.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing into a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// `{pid}-{n}`, distinct for every download this process makes.
fn unique_suffix() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// The hub fetch goes over the network and the token with it; a plain-HTTP
/// endpoint is noted once at startup, since a mirror on a LAN is a real
/// setup and not something to nag about on every file.
fn warn_once_if_insecure(endpoint: &str) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !endpoint.starts_with("https://") && !WARNED.swap(true, Ordering::Relaxed) {
        warn!(
            endpoint,
            "HF_ENDPOINT is not https; model downloads are unencrypted"
        );
    }
}

/// A revision as a branch, tag or commit: letters, digits, `.`, `_` and
/// `-`, and never `..`. It is joined into the cache path and the hub URL, so
/// anything else is refused rather than escaping either.
pub(crate) fn is_valid_revision(revision: &str) -> bool {
    !revision.is_empty()
        && !revision.contains("..")
        && revision
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub(crate) fn check_revision(revision: &str) -> Result<(), EmbedError> {
    if is_valid_revision(revision) {
        Ok(())
    } else {
        Err(EmbedError::Download(format!(
            "EMBEDDING_MODEL_REVISION {revision:?} is not a branch, tag or commit name"
        )))
    }
}

/// `HF_HUB_OFFLINE` set to a true value.
pub(crate) fn offline() -> bool {
    omnimem_core::env::var("HF_HUB_OFFLINE").is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

pub(crate) fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn io(path: &Path, source: std::io::Error) -> EmbedError {
    EmbedError::Io {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const COMMIT: &str = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41";

    /// A tiny HTTP server answering fixed paths (path, status, extra header
    /// lines, body), counting requests.
    fn serve(routes: Vec<(String, u16, String, Vec<u8>)>) -> (String, Arc<AtomicUsize>) {
        let (addr, hits, _) = serve_logged(routes);
        (addr, hits)
    }

    /// `serve`, also logging each request as "<path> token" or "<path>" by
    /// whether it carried an Authorization header. `{port}` in a route's
    /// headers becomes the server's port, so a redirect can name the same
    /// server under another host (`localhost`), as the hub names its CDN.
    fn serve_logged(
        routes: Vec<(String, u16, String, Vec<u8>)>,
    ) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port().to_string();
        let addr = format!("http://127.0.0.1:{port}");
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let log = Arc::new(Mutex::new(Vec::new()));
        let requests = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut token = false;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap() == 0 || header == "\r\n" {
                        break;
                    }
                    token |= header.to_ascii_lowercase().starts_with("authorization:");
                }
                counter.fetch_add(1, Ordering::SeqCst);
                let path = request_line.split_whitespace().nth(1).unwrap_or("");
                requests.lock().unwrap().push(if token {
                    format!("{path} token")
                } else {
                    path.to_owned()
                });
                let (status, extra, body) =
                    routes.iter().find(|(p, _, _, _)| p == path).map_or_else(
                        || (404, String::new(), b"not found".to_vec()),
                        |(_, s, h, b)| (*s, h.replace("{port}", &port), b.clone()),
                    );
                if path == "/drip" {
                    let _ = stream.write_all(b"HTTP/1.1 200 X\r\nConnection: close\r\n\r\n");
                    while stream.write_all(b"a").is_ok() {
                        let _ = stream.flush();
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    continue;
                }
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        (addr, hits, log)
    }

    fn plain(path: String, status: u16, body: Vec<u8>) -> (String, u16, String, Vec<u8>) {
        (path, status, String::new(), body)
    }

    fn scratch() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = env::temp_dir().join(format!(
            "omnimem-download-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    #[test]
    fn commits_resolve_without_a_request() {
        let dl = Downloader::new("http://127.0.0.1:9", None, scratch()).unwrap();
        assert_eq!(dl.resolve_commit("o/m", COMMIT).unwrap(), COMMIT);
    }

    #[test]
    fn a_branch_resolves_through_the_api_and_is_recorded() {
        let body = format!(r#"{{"sha": "{COMMIT}"}}"#).into_bytes();
        let (endpoint, _) = serve(vec![plain(
            "/api/models/o/m/revision/main".into(),
            200,
            body,
        )]);
        let cache = scratch();
        let dl = Downloader::new(&endpoint, None, cache.clone()).unwrap();
        assert_eq!(dl.resolve_commit("o/m", "main").unwrap(), COMMIT);
        let recorded = fs::read_to_string(cache.join("models--o--m/refs/main")).unwrap();
        assert_eq!(recorded, COMMIT);
    }

    #[test]
    fn revisions_that_could_escape_the_cache_are_refused() {
        for good in ["main", "v1.2.3", "refs-heads_x", "abc123", COMMIT] {
            assert!(is_valid_revision(good), "{good}");
        }
        for bad in [
            "",
            "..",
            "../../etc",
            "a/b",
            "main?x=1",
            "a b",
            "a\nb",
            "ma..in",
        ] {
            assert!(!is_valid_revision(bad), "{bad:?}");
        }
        let (endpoint, hits) = serve(vec![]);
        let dl = Downloader::new(&endpoint, None, scratch()).unwrap();
        assert!(matches!(
            dl.resolve_commit("o/m", "../../escape"),
            Err(EmbedError::Download(_))
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 0, "refused before any request");
    }

    #[test]
    fn files_download_once_into_the_snapshot() {
        let path = format!("/o/m/resolve/{COMMIT}/onnx/model.onnx");
        let (endpoint, hits) = serve(vec![plain(path, 200, b"graph bytes".to_vec())]);
        let cache = scratch();
        let dl = Downloader::new(&endpoint, None, cache.clone()).unwrap();
        let Fetched::Present(file) = dl.fetch("o/m", COMMIT, "onnx/model.onnx").unwrap() else {
            panic!("expected the file")
        };
        assert_eq!(fs::read(&file).unwrap(), b"graph bytes");
        assert_eq!(
            file,
            cache.join(format!("models--o--m/snapshots/{COMMIT}/onnx/model.onnx"))
        );
        assert!(matches!(
            dl.fetch("o/m", COMMIT, "onnx/model.onnx").unwrap(),
            Fetched::Present(_)
        ));
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "a cached file makes no request"
        );
        let leftovers: Vec<_> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("incomplete"))
            .collect();
        assert!(leftovers.is_empty(), "the partial file was renamed away");
    }

    #[test]
    fn a_matching_sha256_etag_is_accepted_and_a_wrong_one_refused() {
        let good = format!("/o/m/resolve/{COMMIT}/good.bin");
        let bad = format!("/o/m/resolve/{COMMIT}/bad.bin");
        let short = format!("/o/m/resolve/{COMMIT}/short.bin");
        let (endpoint, _) = serve(vec![
            (
                good,
                200,
                format!("X-Linked-Etag: \"{}\"\r\n", sha256_hex(b"payload")),
                b"payload".to_vec(),
            ),
            (
                bad,
                200,
                format!("ETag: W/\"{}\"\r\n", sha256_hex(b"something else")),
                b"payload".to_vec(),
            ),
            (
                short,
                200,
                "ETag: \"0123456789abcdef0123456789abcdef01234567\"\r\n".to_owned(),
                b"payload".to_vec(),
            ),
        ]);
        let cache = scratch();
        let dl = Downloader::new(&endpoint, None, cache.clone()).unwrap();
        assert!(matches!(
            dl.fetch("o/m", COMMIT, "good.bin").unwrap(),
            Fetched::Present(_)
        ));
        let Err(EmbedError::Download(message)) = dl.fetch("o/m", COMMIT, "bad.bin") else {
            panic!("a wrong sha256 must be an error")
        };
        assert!(message.contains("does not match"), "{message}");
        let snapshot = cache.join(format!("models--o--m/snapshots/{COMMIT}"));
        assert!(
            !snapshot.join("bad.bin").exists(),
            "nothing was renamed into place"
        );
        let leftovers: Vec<_> = fs::read_dir(&snapshot)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("incomplete"))
            .collect();
        assert!(leftovers.is_empty(), "the refused partial was removed");
        // A git blob etag is not a sha256 and is not checked.
        assert!(matches!(
            dl.fetch("o/m", COMMIT, "short.bin").unwrap(),
            Fetched::Present(_)
        ));
    }

    /// The hub answers an LFS file with a redirect naming its sha256 in
    /// `x-linked-etag`; the CDN then sends the bytes with an etag of its own
    /// (a Xet content hash, also 64 hex, not their sha256). Taking the
    /// CDN's etag refused every LFS download, the model included.
    #[test]
    fn an_lfs_file_is_checked_against_the_hub_not_the_cdn() {
        let good = format!("/o/m/resolve/{COMMIT}/model.onnx");
        let bad = format!("/o/m/resolve/{COMMIT}/bad.onnx");
        let cdn_etag = format!("ETag: \"{}\"\r\n", sha256_hex(b"xet content hash"));
        let redirect = |sha: String, to: &str| {
            format!("X-Linked-Etag: \"{sha}\"\r\nLocation: http://localhost:{{port}}/cdn/{to}\r\n")
        };
        let (endpoint, _, log) = serve_logged(vec![
            (
                good,
                302,
                redirect(sha256_hex(b"weights"), "good"),
                Vec::new(),
            ),
            (bad, 302, redirect(sha256_hex(b"other"), "bad"), Vec::new()),
            (
                "/cdn/good".into(),
                200,
                cdn_etag.clone(),
                b"weights".to_vec(),
            ),
            ("/cdn/bad".into(), 200, cdn_etag, b"weights".to_vec()),
        ]);
        let dl = Downloader::new(&endpoint, Some("hf_secret".into()), scratch()).unwrap();

        let Fetched::Present(file) = dl.fetch("o/m", COMMIT, "model.onnx").unwrap() else {
            panic!("expected the file")
        };
        assert_eq!(fs::read(&file).unwrap(), b"weights");
        assert_eq!(
            *log.lock().unwrap(),
            [
                format!("/o/m/resolve/{COMMIT}/model.onnx token"),
                "/cdn/good".to_owned()
            ],
            "the token goes to the hub and not to the CDN"
        );

        let Err(EmbedError::Download(message)) = dl.fetch("o/m", COMMIT, "bad.onnx") else {
            panic!("a sha256 the hub didn't name must be refused")
        };
        assert!(message.contains("does not match"), "{message}");
    }

    #[test]
    fn oversized_and_drip_fed_bodies_are_refused() {
        let big = format!("/o/m/resolve/{COMMIT}/big.bin");
        let (endpoint, _) = serve(vec![plain(big, 200, vec![b'x'; 100])]);
        let mut dl = Downloader::new(&endpoint, None, scratch()).unwrap();
        dl.max_file_bytes = 50;
        let Err(EmbedError::Download(message)) = dl.fetch("o/m", COMMIT, "big.bin") else {
            panic!("an oversized body must be an error")
        };
        assert!(message.contains("more than 50 bytes"), "{message}");

        let (endpoint, _) = serve(vec![]);
        let mut dl = Downloader::new(&endpoint, None, scratch()).unwrap();
        dl.endpoint = endpoint.clone();
        dl.budget = Duration::from_millis(400);
        // `/drip` answers any repo path that ends in it, so the URL is
        // built to land on the drip route.
        let started = Instant::now();
        let mut response = dl.get(&format!("{endpoint}/drip")).unwrap();
        let partial = scratch().join("drip.incomplete");
        let Err(EmbedError::Download(message)) =
            dl.copy_body(&format!("{endpoint}/drip"), &mut response, &partial, None)
        else {
            panic!("a drip-fed body must run out of budget")
        };
        assert!(message.contains("budget"), "{message}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_missing_file_is_absent_and_remembered() {
        let (endpoint, hits) = serve(vec![]);
        let cache = scratch();
        let dl = Downloader::new(&endpoint, None, cache.clone()).unwrap();
        assert!(matches!(
            dl.fetch("o/m", COMMIT, "2_Normalize/config.json").unwrap(),
            Fetched::Absent
        ));
        assert!(
            cache
                .join(format!(
                    "models--o--m/.no_exist/{COMMIT}/2_Normalize/config.json"
                ))
                .is_file()
        );
        assert!(matches!(
            dl.fetch("o/m", COMMIT, "2_Normalize/config.json").unwrap(),
            Fetched::Absent
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_server_error_is_a_download_error_not_absence() {
        let path = format!("/o/m/resolve/{COMMIT}/tokenizer.json");
        let (endpoint, _) = serve(vec![plain(path, 503, b"busy".to_vec())]);
        let dl = Downloader::new(&endpoint, None, scratch()).unwrap();
        assert!(matches!(
            dl.fetch("o/m", COMMIT, "tokenizer.json"),
            Err(EmbedError::Download(_))
        ));
    }

    #[test]
    fn etags_are_read_only_as_sha256() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(expected_sha256(&headers), None);
        headers.insert(
            "etag",
            "\"0123456789abcdef0123456789abcdef01234567\""
                .parse()
                .unwrap(),
        );
        assert_eq!(expected_sha256(&headers), None);
        let digest = sha256_hex(b"x");
        headers.insert(
            "x-linked-etag",
            format!("W/\"{}\"", digest.to_ascii_uppercase())
                .parse()
                .unwrap(),
        );
        assert_eq!(expected_sha256(&headers).as_deref(), Some(digest.as_str()));
    }
}
