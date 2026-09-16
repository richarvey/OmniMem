//! Configuration and model file resolution.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tracing::{info, warn};

use crate::download::{self, Downloader, Fetched};
use crate::engine::Pooling;
use crate::error::EmbedError;

pub const DEFAULT_MODEL: &str = "all-MiniLM-L6-v2";
pub const DEFAULT_ONNX_FILE: &str = "onnx/model.onnx";
/// The sentence-transformers/all-MiniLM-L6-v2 commit the 6.7 numbers were
/// measured against, and the one `reference_vectors.json` was generated from.
pub const DEFAULT_MODEL_REVISION: &str = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41";
pub const DEFAULT_MAX_SEQ_LENGTH: usize = 256;
/// Two special tokens plus at least one token of text.
const MIN_SEQ_LENGTH: usize = 3;

pub(crate) const TOKENIZER_FILE: &str = "tokenizer.json";
const SBERT_CONFIG_FILE: &str = "sentence_bert_config.json";
const POOLING_CONFIG_FILE: &str = "1_Pooling/config.json";
const MODEL_CONFIG_FILE: &str = "config.json";

/// What to load. Mirrors the 6.x environment variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedConfig {
    /// `EMBEDDING_MODEL`: a repo name (bare names live under
    /// `sentence-transformers/`), `owner/name`, or a local directory.
    pub model: String,
    /// `EMBEDDING_ONNX_FILE`
    pub onnx_file: String,
    /// `EMBEDDING_MODEL_REVISION`. `None` means the repo's `main`, except
    /// for the default model, which is pinned.
    pub revision: Option<String>,
    /// `EMBEDDING_MAX_SEQ_LENGTH`; `None` reads the model's own.
    pub max_seq_length: Option<usize>,
    /// `EMBEDDING_THREADS`; `None` leaves ONNX Runtime's default.
    pub threads: Option<usize>,
}

impl Default for EmbedConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_owned(),
            onnx_file: DEFAULT_ONNX_FILE.to_owned(),
            revision: None,
            max_seq_length: None,
            threads: None,
        }
    }
}

impl EmbedConfig {
    pub fn from_env() -> Self {
        let text = |name: &str| {
            omnimem_core::env::var(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let number = |name: &str| {
            text(name).and_then(|raw| {
                if let Ok(n) = raw.parse::<usize>() {
                    Some(n)
                } else {
                    warn!("{name}={raw:?} is not a whole number; ignoring it");
                    None
                }
            })
        };
        Self {
            model: text("EMBEDDING_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
            onnx_file: text("EMBEDDING_ONNX_FILE").unwrap_or_else(|| DEFAULT_ONNX_FILE.to_owned()),
            revision: text("EMBEDDING_MODEL_REVISION"),
            max_seq_length: number("EMBEDDING_MAX_SEQ_LENGTH"),
            threads: number("EMBEDDING_THREADS"),
        }
    }

    /// `all-MiniLM-L6-v2` becomes `sentence-transformers/all-MiniLM-L6-v2`;
    /// a name with an owner, or a local directory, is used as given.
    pub fn repo(&self) -> String {
        let model = self.model.trim();
        if Path::new(model).is_dir() || model.contains('/') {
            model.to_owned()
        } else {
            format!("sentence-transformers/{model}")
        }
    }

    /// The revision actually fetched: the explicit one, else the pin for
    /// the default model, else `main`.
    pub fn effective_revision(&self) -> String {
        if let Some(rev) = &self.revision {
            return rev.clone();
        }
        let default_repo = Self::default().repo();
        if self.repo() == default_repo {
            DEFAULT_MODEL_REVISION.to_owned()
        } else {
            "main".to_owned()
        }
    }
}

/// Where a model's files live on disk.
pub(crate) struct ModelFiles {
    repo: String,
    root: PathBuf,
}

impl ModelFiles {
    /// A local directory, the cached snapshot, or a fresh download of the
    /// files the engine reads. Downloading needs the network and is skipped
    /// under `HF_HUB_OFFLINE`.
    pub(crate) fn locate(config: &EmbedConfig) -> Result<Self, EmbedError> {
        let repo = config.repo();
        if Path::new(&repo).is_dir() {
            return Ok(Self {
                root: PathBuf::from(&repo),
                repo,
            });
        }
        let revision = config.effective_revision();
        // The revision is joined into cache paths and hub URLs below, so it
        // is checked before either sees it.
        download::check_revision(&revision)?;
        if let Some(snapshot) = hf_snapshot_dir(&repo, &revision) {
            return Ok(Self {
                repo,
                root: snapshot,
            });
        }
        if download::offline() {
            return Err(EmbedError::ModelUnavailable {
                repo,
                file: config.onnx_file.clone(),
                detail: format!(
                    "revision {revision} is not in the Hugging Face cache at {} and HF_HUB_OFFLINE is set",
                    hub_cache_dir().display()
                ),
            });
        }

        let downloader = Downloader::from_env(hub_cache_dir())?;
        let commit = downloader.resolve_commit(&repo, &revision)?;
        info!(%repo, %revision, %commit, "model not cached; downloading");
        for file in [config.onnx_file.as_str(), TOKENIZER_FILE] {
            if let Fetched::Absent = downloader.fetch(&repo, &commit, file)? {
                return Err(EmbedError::ModelUnavailable {
                    repo: repo.clone(),
                    file: file.to_owned(),
                    detail: "the repository has no such file".to_owned(),
                });
            }
        }
        for file in [POOLING_CONFIG_FILE, SBERT_CONFIG_FILE, MODEL_CONFIG_FILE] {
            downloader.fetch(&repo, &commit, file)?;
        }
        let root = hub_cache_dir()
            .join(format!("models--{}", repo.replace('/', "--")))
            .join("snapshots")
            .join(&commit);
        Ok(Self { repo, root })
    }

    pub(crate) fn repo(&self) -> &str {
        &self.repo
    }

    /// The commit this snapshot is, when it lives in the hub cache.
    fn snapshot_commit(&self) -> Option<String> {
        let parent = self.root.parent()?;
        (parent.file_name()? == "snapshots")
            .then(|| {
                self.root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .flatten()
    }

    /// A file the engine can't run without. A cached snapshot missing it
    /// (one filled by the pre-6.7 torch backend holds the weights but not the
    /// ONNX graph) fetches it, unless offline.
    pub(crate) fn required(&self, file: &str) -> Result<PathBuf, EmbedError> {
        let path = self.root.join(file);
        if path.is_file() {
            return Ok(path);
        }
        if let Some(commit) = self.snapshot_commit()
            && !download::offline()
        {
            let downloader = Downloader::from_env(hub_cache_dir())?;
            if let Fetched::Present(fetched) = downloader.fetch(&self.repo, &commit, file)? {
                return Ok(fetched);
            }
        }
        Err(EmbedError::ModelUnavailable {
            repo: self.repo.clone(),
            file: file.to_owned(),
            detail: format!("{} does not exist", path.display()),
        })
    }

    /// A repo config file, or `None` when the repo lacks it or it's unreadable.
    fn optional_json(&self, file: &str) -> Option<Value> {
        let path = self.root.join(file);
        let raw = fs::read_to_string(&path).ok()?;
        match serde_json::from_str::<Value>(&raw) {
            Ok(value) if value.is_object() => Some(value),
            _ => {
                warn!("unreadable {file} for {}; ignoring it", self.repo);
                None
            }
        }
    }

    /// The pooling mode from `1_Pooling/config.json`.
    pub(crate) fn pooling(&self) -> Result<Pooling, EmbedError> {
        let Some(config) = self.optional_json(POOLING_CONFIG_FILE) else {
            warn!(
                "no {POOLING_CONFIG_FILE} for {}; assuming mean pooling",
                self.repo
            );
            return Ok(Pooling::Mean);
        };
        let known = [
            (Pooling::Mean, "pooling_mode_mean_tokens"),
            (Pooling::Cls, "pooling_mode_cls_token"),
            (Pooling::Max, "pooling_mode_max_tokens"),
        ];
        let on = |key: &str| config.get(key).is_some_and(truthy);
        let enabled: Vec<Pooling> = known
            .iter()
            .filter(|(_, key)| on(key))
            .map(|(mode, _)| *mode)
            .collect();
        let mut others: Vec<String> = config
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, value)| {
                key.starts_with("pooling_mode_")
                    && truthy(value)
                    && !known.iter().any(|(_, k)| k == key)
            })
            .map(|(key, _)| key.clone())
            .collect();
        others.sort();
        if enabled.len() != 1 || !others.is_empty() {
            let described = if !others.is_empty() {
                format!("{others:?}")
            } else if enabled.is_empty() {
                "no pooling mode at all".to_owned()
            } else {
                format!("{enabled:?}")
            };
            return Err(EmbedError::UnsupportedPooling {
                repo: self.repo.clone(),
                described,
            });
        }
        Ok(enabled[0])
    }

    /// The token cap: the override or the model's own, at least three, and
    /// no more than the graph's position table.
    pub(crate) fn max_seq_length(&self, requested: Option<usize>) -> usize {
        let mut value = match requested {
            Some(n) => n,
            None => self
                .optional_json(SBERT_CONFIG_FILE)
                .and_then(|c| c.get("max_seq_length").and_then(Value::as_u64))
                .map_or(DEFAULT_MAX_SEQ_LENGTH, |n| n as usize),
        };
        if value < MIN_SEQ_LENGTH {
            warn!(
                "max_seq_length {value} is below {MIN_SEQ_LENGTH}; using {DEFAULT_MAX_SEQ_LENGTH}"
            );
            value = DEFAULT_MAX_SEQ_LENGTH;
        }
        let positions = self
            .optional_json(MODEL_CONFIG_FILE)
            .and_then(|c| c.get("max_position_embeddings").and_then(Value::as_u64))
            .unwrap_or(0) as usize;
        if positions > 0 && value > positions {
            warn!("max_seq_length {value} exceeds the model's {positions} positions; clamping");
            value = positions;
        }
        value
    }
}

/// JSON truthiness as Python's `if config.get(key)` sees it.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// The Hugging Face hub cache: `HF_HUB_CACHE`, else `HF_HOME/hub`, else
/// `XDG_CACHE_HOME/huggingface/hub`, else `~/.cache/huggingface/hub`.
pub(crate) fn hub_cache_dir() -> PathBuf {
    if let Some(dir) = env::var_os("HF_HUB_CACHE").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    if let Some(home) = env::var_os("HF_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join("hub");
    }
    let cache = env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from(".cache"));
    cache.join("huggingface").join("hub")
}

/// `models--{owner}--{name}/snapshots/{commit}`, resolving a branch name
/// through `refs/` the way `huggingface_hub` does.
fn hf_snapshot_dir(repo: &str, revision: &str) -> Option<PathBuf> {
    let base = hub_cache_dir().join(format!("models--{}", repo.replace('/', "--")));
    let direct = base.join("snapshots").join(revision);
    if direct.is_dir() {
        return Some(direct);
    }
    let commit = fs::read_to_string(base.join("refs").join(revision)).ok()?;
    let resolved = base.join("snapshots").join(commit.trim());
    resolved.is_dir().then_some(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_names_resolve_under_sentence_transformers() {
        let config = EmbedConfig {
            model: "all-MiniLM-L6-v2".into(),
            ..EmbedConfig::default()
        };
        assert_eq!(config.repo(), "sentence-transformers/all-MiniLM-L6-v2");
        let owned = EmbedConfig {
            model: "BAAI/bge-small-en".into(),
            ..EmbedConfig::default()
        };
        assert_eq!(owned.repo(), "BAAI/bge-small-en");
    }

    #[test]
    fn only_the_default_model_is_pinned() {
        assert_eq!(
            EmbedConfig::default().effective_revision(),
            DEFAULT_MODEL_REVISION
        );
        let other = EmbedConfig {
            model: "BAAI/bge-small-en".into(),
            ..EmbedConfig::default()
        };
        assert_eq!(other.effective_revision(), "main");
        let explicit = EmbedConfig {
            revision: Some("abc123".into()),
            ..EmbedConfig::default()
        };
        assert_eq!(explicit.effective_revision(), "abc123");
    }

    #[test]
    fn python_truthiness() {
        assert!(truthy(&Value::Bool(true)));
        assert!(!truthy(&Value::Bool(false)));
        assert!(!truthy(&serde_json::json!(0)));
        assert!(truthy(&serde_json::json!(1)));
        assert!(!truthy(&Value::Null));
    }

    fn scratch_model(files: &[(&str, &str)]) -> (tempdir::Dir, ModelFiles) {
        let dir = tempdir::Dir::new();
        for (name, body) in files {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        let files = ModelFiles {
            repo: "local".into(),
            root: dir.path().to_owned(),
        };
        (dir, files)
    }

    #[test]
    fn pooling_follows_the_model_config() {
        let (_d, files) =
            scratch_model(&[(POOLING_CONFIG_FILE, r#"{"pooling_mode_cls_token": true}"#)]);
        assert_eq!(files.pooling().unwrap(), Pooling::Cls);
        let (_d, files) = scratch_model(&[]);
        assert_eq!(files.pooling().unwrap(), Pooling::Mean);
    }

    #[test]
    fn unsupported_or_ambiguous_pooling_is_refused() {
        let (_d, files) = scratch_model(&[(
            POOLING_CONFIG_FILE,
            r#"{"pooling_mode_weightedmean_tokens": true}"#,
        )]);
        assert!(matches!(
            files.pooling(),
            Err(EmbedError::UnsupportedPooling { .. })
        ));
        let (_d, files) = scratch_model(&[(
            POOLING_CONFIG_FILE,
            r#"{"pooling_mode_mean_tokens": true, "pooling_mode_max_tokens": true}"#,
        )]);
        assert!(matches!(
            files.pooling(),
            Err(EmbedError::UnsupportedPooling { .. })
        ));
    }

    #[test]
    fn max_seq_length_is_bounded() {
        let (_d, files) = scratch_model(&[
            (SBERT_CONFIG_FILE, r#"{"max_seq_length": 256}"#),
            (MODEL_CONFIG_FILE, r#"{"max_position_embeddings": 512}"#),
        ]);
        assert_eq!(files.max_seq_length(None), 256);
        assert_eq!(files.max_seq_length(Some(2)), 256);
        assert_eq!(files.max_seq_length(Some(4096)), 512);
        assert_eq!(files.max_seq_length(Some(128)), 128);
    }

    /// A self-deleting directory, to keep dev-dependencies at zero.
    mod tempdir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU32, Ordering};

        pub struct Dir(PathBuf);

        impl Dir {
            pub fn new() -> Self {
                static N: AtomicU32 = AtomicU32::new(0);
                let path = std::env::temp_dir().join(format!(
                    "omnimem-embed-test-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
