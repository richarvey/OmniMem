//! `/backups`: create, upload, preview, restore, download and delete backup
//! files (`web_ui/routes/backups.py`).
//!
//! Create and restore go through `dump_to_file` and `restore_from_file`, the
//! MCP tools' own paths, so a restore runs every migration and re-embeds what
//! it brings back. Downloads are saved into the Downloads folder.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::PathBuf;

use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use chrono::{DateTime, Local};
use minijinja::context;
use omnimem_engine::pyfmt::round_to;
use serde_json::{Value, json};
use tracing::{error, info};

use crate::PanelState;
use crate::files::{read_upload, save_download};
use crate::pages::{blocking, off_runtime, quote, see_other, starting};
use crate::render::page;

/// Uploads and restores are capped as the MCP restore tool caps them.
const MAX_BACKUP_BYTES: usize = 100 * 1024 * 1024;

/// True for a name ending in `.json` exactly. The check is case-sensitive
/// on purpose: the engine's backup rule is, so a `.JSON` accepted here would
/// be a file the restore tool then refuses.
fn has_json_extension(filename: &str) -> bool {
    filename
        .rsplit_once('.')
        .is_some_and(|(_, ext)| ext == "json")
}

/// A plain `<name>.json`: letters, digits, `_`, `-` and `.` only, so a
/// request can't name anything outside the backup folder.
fn is_safe_filename(filename: &str) -> bool {
    filename.len() > ".json".len()
        && filename.len() <= 255
        && has_json_extension(filename)
        && filename
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn with_message(problem: bool, text: &str) -> Response {
    let field = if problem { "error" } else { "message" };
    see_other(&format!("/backups?{field}={}", quote(text)))
}

fn backup_dir(state: &PanelState) -> Option<PathBuf> {
    state.engine().map(|e| e.config().backup_dir.clone())
}

fn listing(dir: &std::path::Path) -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut backups: Vec<Value> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json") && e.path().is_file())
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let modified = meta
                .modified()
                .ok()
                .map(|t| {
                    DateTime::<Local>::from(t)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default();
            Some(json!({
                "filename": e.file_name().to_string_lossy(),
                "size_kb": round_to(meta.len() as f64 / 1024.0, 2),
                "modified": modified,
            }))
        })
        .collect();
    backups.sort_by(|a, b| b["modified"].as_str().cmp(&a["modified"].as_str()));
    backups
}

pub(crate) async fn list(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(dir) = backup_dir(&state) else {
        return starting(&state);
    };
    // A directory scan with a stat per file: off the workers.
    let backups = match off_runtime(move || listing(&dir)).await {
        Ok(backups) => backups,
        Err(failure) => return failure,
    };
    let param = |name: &str| query.get(name).cloned().unwrap_or_default();
    page(
        state.templates(),
        "backups.html",
        context! {
            current_page => "backups",
            backups,
            message => param("message"),
            error => param("error"),
        },
    )
}

pub(crate) async fn create(State(state): State<PanelState>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let result = blocking(move || {
        std::fs::create_dir_all(&engine.config().backup_dir).map_err(|e| {
            omnimem_engine::EngineError::Io(format!("creating the backup folder: {e}"))
        })?;
        engine.dump_to_file(None)
    })
    .await;
    match result {
        Ok(result) if result["status"] == "error" => with_message(
            true,
            &format!(
                "Backup failed: {}",
                result["message"].as_str().unwrap_or("")
            ),
        ),
        Ok(result) => with_message(
            false,
            &format!(
                "Backup created: {} ({} keys)",
                result["filename"].as_str().unwrap_or(""),
                result["total_keys"]
            ),
        ),
        Err(failure) => failure,
    }
}

pub(crate) async fn upload(State(state): State<PanelState>, mut multipart: Multipart) -> Response {
    let Some(dir) = backup_dir(&state) else {
        return starting(&state);
    };
    let (filename, data) = match read_upload(&mut multipart, "file").await {
        Ok(Some(upload)) if !upload.0.is_empty() => upload,
        Ok(_) => return with_message(true, "No file selected"),
        Err(problem) => return with_message(true, &problem),
    };
    if !has_json_extension(&filename) {
        return with_message(true, "Only .json files are allowed");
    }
    // Keep only the base name, and only safe characters in it.
    let base = filename.rsplit(['/', '\\']).next().unwrap_or("");
    let mut safe: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    if !has_json_extension(&safe) {
        safe.push_str(".json");
    }
    if !is_safe_filename(&safe) {
        return with_message(true, "Invalid filename");
    }
    if data.len() > MAX_BACKUP_BYTES {
        return with_message(
            true,
            &format!("File too large (max {} MB)", MAX_BACKUP_BYTES / 1024 / 1024),
        );
    }
    if serde_json::from_slice::<Value>(&data).is_err() {
        return with_message(true, "File is not valid JSON");
    }
    let path = dir.join(&safe);
    let bytes = data.len();
    // `create_new` claims the name and writes in one step, so an upload can
    // never replace a backup that is already there. Up to 100 MB is written,
    // so it goes off the workers.
    let written = off_runtime(move || {
        std::fs::create_dir_all(&dir).and_then(|()| {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .and_then(|mut file| file.write_all(&data))
        })
    })
    .await;
    match written {
        Ok(Ok(())) => {
            info!(file = safe, bytes, "uploaded a backup");
            with_message(false, &format!("Uploaded {safe}"))
        }
        Ok(Err(e)) if e.kind() == ErrorKind::AlreadyExists => with_message(
            true,
            &format!("A backup named {safe} already exists. Rename the file and upload it again."),
        ),
        Ok(Err(e)) => {
            error!(error = %e, "backup upload failed");
            with_message(true, &format!("Upload failed: {e}"))
        }
        Err(failure) => failure,
    }
}

fn plain(status: StatusCode, text: &'static str) -> Response {
    (
        status,
        Html(format!(r#"<p class="empty-state">{text}</p>"#)),
    )
        .into_response()
}

/// A named backup that exists, or the answer to give instead.
fn existing(
    state: &PanelState,
    filename: &str,
    page_errors: bool,
) -> Result<PathBuf, Box<Response>> {
    let Some(dir) = backup_dir(state) else {
        return Err(Box::new(starting(state)));
    };
    if !is_safe_filename(filename) {
        return Err(Box::new(if page_errors {
            plain(StatusCode::BAD_REQUEST, "Invalid filename.")
        } else {
            with_message(true, "Invalid filename")
        }));
    }
    let path = dir.join(filename);
    if !path.is_file() {
        return Err(Box::new(if page_errors {
            plain(StatusCode::NOT_FOUND, "Backup file not found.")
        } else {
            with_message(true, "Backup file not found")
        }));
    }
    Ok(path)
}

pub(crate) async fn preview(
    State(state): State<PanelState>,
    Path(filename): Path<String>,
) -> Response {
    if let Err(response) = existing(&state, &filename, true) {
        return *response;
    }
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let name = filename.clone();
    match blocking(move || engine.restore_from_file(&name, true)).await {
        Ok(result) if result["status"] == "dry_run" => page(
            state.templates(),
            "backups.html",
            context! {
                current_page => "backups",
                backups => Vec::<Value>::new(),
                message => "",
                error => "",
                preview => json!({
                    "filename": filename,
                    "total_keys": result["total_keys_in_backup"],
                    "metadata": result["metadata"],
                }),
            },
        ),
        Ok(_) => plain(StatusCode::BAD_REQUEST, "Invalid backup file."),
        Err(failure) => failure,
    }
}

pub(crate) async fn restore(
    State(state): State<PanelState>,
    Path(filename): Path<String>,
) -> Response {
    if let Err(response) = existing(&state, &filename, false) {
        return *response;
    }
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let name = filename.clone();
    match blocking(move || engine.restore_from_file(&name, false)).await {
        Ok(result) if result["status"] == "restored" => {
            info!(file = filename, restored = %result["restored_keys"], "restored a backup");
            with_message(
                false,
                &format!(
                    "Restored {} keys from {filename} (skipped {})",
                    result["restored_keys"], result["skipped_keys"]
                ),
            )
        }
        Ok(result) => with_message(
            true,
            &format!(
                "Restore failed: {}",
                result["message"].as_str().unwrap_or("unknown error")
            ),
        ),
        Err(failure) => failure,
    }
}

pub(crate) async fn download(
    State(state): State<PanelState>,
    Path(filename): Path<String>,
) -> Response {
    let path = match existing(&state, &filename, false) {
        Ok(path) => path,
        Err(response) => return *response,
    };
    let downloads = state.downloads_dir();
    let saved = off_runtime(move || {
        std::fs::read(&path)
            .map_err(|e| format!("Could not read {filename}: {e}"))
            .and_then(|data| save_download(downloads, &filename, &data))
    })
    .await;
    match saved {
        Ok(Ok(saved)) => with_message(false, &format!("Saved a copy to {}.", saved.display())),
        Ok(Err(problem)) => with_message(true, &problem),
        Err(failure) => failure,
    }
}

pub(crate) async fn delete(
    State(state): State<PanelState>,
    Path(filename): Path<String>,
) -> Response {
    let path = match existing(&state, &filename, false) {
        Ok(path) => path,
        Err(response) => return *response,
    };
    match off_runtime(move || std::fs::remove_file(&path)).await {
        Ok(Ok(())) => {
            info!(file = filename, "deleted a backup");
            with_message(false, &format!("Deleted {filename}"))
        }
        Ok(Err(e)) => with_message(true, &format!("Delete failed: {e}")),
        Err(failure) => failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_json_names_are_accepted() {
        assert!(is_safe_filename("memory_backup_20260915_120000.json"));
        assert!(!is_safe_filename(".json"));
        assert!(!is_safe_filename("../secrets.json"));
        assert!(!is_safe_filename("notes.txt"));
        assert!(!is_safe_filename("spaced name.json"));
    }
}
