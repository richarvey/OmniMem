//! Files the panel hands over or takes in.
//!
//! A webview can't hand a download to the person the same way on every
//! platform (WebKitGTK saves it silently, macOS won't say where), so a file
//! someone asks for is saved into their Downloads folder, never over an
//! existing file, and the page says where it went. Uploads arrive as
//! multipart forms.

use std::path::{Path, PathBuf};

use axum::extract::Multipart;

/// A path in `dir` for `filename` that doesn't overwrite anything:
/// `name.zip`, then `name (1).zip` and so on.
pub(crate) fn free_path(dir: &Path, filename: &str) -> PathBuf {
    let candidate = dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = filename
        .rsplit_once('.')
        .map_or((filename, String::new()), |(s, e)| (s, format!(".{e}")));
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(candidate)
}

/// Save `data` into the downloads folder as `filename`, or a free variant.
pub(crate) fn save_download(
    dir: Option<PathBuf>,
    filename: &str,
    data: &[u8],
) -> Result<PathBuf, String> {
    let dir = dir.ok_or_else(|| "There is no Downloads folder to save the file in.".to_owned())?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let path = free_path(&dir, filename);
    std::fs::write(&path, data).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    Ok(path)
}

/// The first uploaded file in the form field `name`: its filename (empty
/// when none was chosen) and bytes.
pub(crate) async fn read_upload(
    multipart: &mut Multipart,
    name: &str,
) -> Result<Option<(String, Vec<u8>)>, String> {
    let mut upload = None;
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) if upload.is_none() && field.name() == Some(name) => {
                let filename = field.file_name().unwrap_or("").to_owned();
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| format!("The upload didn't arrive whole: {e}"))?;
                upload = Some((filename, data.to_vec()));
            }
            Ok(Some(_)) => {}
            Ok(None) => return Ok(upload),
            Err(e) => return Err(format!("The upload didn't arrive whole: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloads_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("omnimem-files-{}", ulid::Ulid::generate()));
        assert!(save_download(None, "x.zip", b"x").is_err());
        let first = save_download(Some(dir.clone()), "rust.zip", b"one").unwrap();
        let second = save_download(Some(dir.clone()), "rust.zip", b"two").unwrap();
        assert_eq!(first, dir.join("rust.zip"));
        assert_eq!(second, dir.join("rust (1).zip"));
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
        assert_eq!(free_path(&dir, "feeds"), dir.join("feeds"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
