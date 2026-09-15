//! Fetching model files from the Hugging Face hub into the shared cache.
//!
//! Files land where `huggingface_hub` would look for them
//! (`models--{owner}--{name}/snapshots/{commit}/{file}`, with `refs/{branch}`
//! naming the commit), so a cache filled by 6.x Python and one filled by this
//! engine are interchangeable. A file the repo doesn't have is recorded under
//! `.no_exist/`, as the Python library does.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tracing::info;

use crate::error::EmbedError;

pub(crate) const DEFAULT_ENDPOINT: &str = "https://huggingface.co";

pub(crate) struct Downloader {
    endpoint: String,
    token: Option<String>,
    cache: PathBuf,
    client: reqwest::blocking::Client,
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
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("omnimem/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(900))
            .build()
            .map_err(|e| EmbedError::Download(e.to_string()))?;
        Ok(Self {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            token,
            cache,
            client,
        })
    }

    /// `HF_ENDPOINT` and `HF_TOKEN`, as the Python library reads them.
    pub(crate) fn from_env(cache: PathBuf) -> Result<Self, EmbedError> {
        let var = |name: &str| {
            env::var(name)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let endpoint = var("HF_ENDPOINT").unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());
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

    /// The commit a revision names. A 40-character commit is used as given;
    /// a branch or tag is asked of the hub and recorded under `refs/`.
    pub(crate) fn resolve_commit(&self, repo: &str, revision: &str) -> Result<String, EmbedError> {
        if is_commit(revision) {
            return Ok(revision.to_owned());
        }
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
        let mut response = self.get(&url)?;
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

        let parent = target
            .parent()
            .ok_or_else(|| EmbedError::Download(format!("bad file name {file}")))?;
        fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
        // Written beside the target and renamed into place, so an interrupted
        // download never leaves a truncated model where the engine looks.
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let partial = parent.join(format!("{name}.incomplete"));
        let mut out = fs::File::create(&partial).map_err(|e| io(&partial, e))?;
        let bytes = response
            .copy_to(&mut out)
            .map_err(|e| EmbedError::Download(format!("{url}: {e}")))?;
        out.flush().map_err(|e| io(&partial, e))?;
        drop(out);
        fs::rename(&partial, &target).map_err(|e| io(&target, e))?;
        info!(repo, file, bytes, "downloaded model file");
        Ok(Fetched::Present(target))
    }
}

/// `HF_HUB_OFFLINE` set to a true value.
pub(crate) fn offline() -> bool {
    env::var("HF_HUB_OFFLINE")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const COMMIT: &str = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41";

    /// A tiny HTTP server answering fixed paths, counting requests.
    fn serve(routes: Vec<(String, u16, Vec<u8>)>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap() == 0 || header == "\r\n" {
                        break;
                    }
                }
                counter.fetch_add(1, Ordering::SeqCst);
                let path = request_line.split_whitespace().nth(1).unwrap_or("");
                let (status, body) = routes
                    .iter()
                    .find(|(p, _, _)| p == path)
                    .map(|(_, s, b)| (*s, b.clone()))
                    .unwrap_or((404, b"not found".to_vec()));
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        (addr, hits)
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

    #[test]
    fn commits_resolve_without_a_request() {
        let dl = Downloader::new("http://127.0.0.1:9", None, scratch()).unwrap();
        assert_eq!(dl.resolve_commit("o/m", COMMIT).unwrap(), COMMIT);
    }

    #[test]
    fn a_branch_resolves_through_the_api_and_is_recorded() {
        let body = format!(r#"{{"sha": "{COMMIT}"}}"#).into_bytes();
        let (endpoint, _) = serve(vec![("/api/models/o/m/revision/main".into(), 200, body)]);
        let cache = scratch();
        let dl = Downloader::new(&endpoint, None, cache.clone()).unwrap();
        assert_eq!(dl.resolve_commit("o/m", "main").unwrap(), COMMIT);
        let recorded = fs::read_to_string(cache.join("models--o--m/refs/main")).unwrap();
        assert_eq!(recorded, COMMIT);
    }

    #[test]
    fn files_download_once_into_the_snapshot() {
        let path = format!("/o/m/resolve/{COMMIT}/onnx/model.onnx");
        let (endpoint, hits) = serve(vec![(path, 200, b"graph bytes".to_vec())]);
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
        let (endpoint, _) = serve(vec![(path, 503, b"busy".to_vec())]);
        let dl = Downloader::new(&endpoint, None, scratch()).unwrap();
        assert!(matches!(
            dl.fetch("o/m", COMMIT, "tokenizer.json"),
            Err(EmbedError::Download(_))
        ));
    }
}
