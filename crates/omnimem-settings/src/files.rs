//! Files the panel hands over or takes in.
//!
//! A webview can't hand a download to the person the same way on every
//! platform (WebKitGTK saves it silently, macOS won't say where), so a file
//! someone asks for is saved into their Downloads folder, never over an
//! existing file, and the page says where it went. Uploads arrive as
//! multipart forms.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::PathBuf;

use axum::extract::Multipart;

/// How many `name (n).ext` variants are tried before giving up: a folder
/// with that many copies of one download is not what anyone wants.
const MAX_VARIANTS: usize = 1000;

/// The names tried in turn for `filename`: `name.zip`, then `name (1).zip`
/// and so on.
fn candidate_names(filename: &str) -> impl Iterator<Item = String> + '_ {
    let (stem, ext) = filename
        .rsplit_once('.')
        .map_or((filename, String::new()), |(s, e)| (s, format!(".{e}")));
    std::iter::once(filename.to_owned())
        .chain((1..MAX_VARIANTS).map(move |n| format!("{stem} ({n}){ext}")))
}

/// Save `data` into the downloads folder as `filename`, or a free variant.
///
/// Each name is claimed with `create_new`, which fails if anything is
/// already there, a symlink included, so the check and the write are one
/// step: nothing can be replaced or written through a link that appeared
/// between looking and writing.
pub(crate) fn save_download(
    dir: Option<PathBuf>,
    filename: &str,
    data: &[u8],
) -> Result<PathBuf, String> {
    let dir = dir.ok_or_else(|| "There is no Downloads folder to save the file in.".to_owned())?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    for name in candidate_names(filename) {
        let path = dir.join(&name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(data)
                    .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("Could not write {}: {e}", path.display())),
        }
    }
    Err(format!(
        "Could not find a free name for {filename} in {}.",
        dir.display()
    ))
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
        assert_eq!(
            save_download(Some(dir.clone()), "feeds", b"x").unwrap(),
            dir.join("feeds"),
            "no extension, no suffix"
        );
        assert_eq!(
            candidate_names("a.b.zip").nth(2).as_deref(),
            Some("a.b (2).zip")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A symlink at the target name is not followed: the download lands
    /// beside it under the next free name and the link's target is untouched.
    #[cfg(unix)]
    #[test]
    fn a_symlink_in_the_way_is_not_written_through() {
        let dir = std::env::temp_dir().join(format!("omnimem-files-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("precious.txt");
        std::fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, dir.join("export.json")).unwrap();
        let dangling = dir.join("gone.json");
        std::os::unix::fs::symlink(dir.join("nowhere"), &dangling).unwrap();

        let saved = save_download(Some(dir.clone()), "export.json", b"new").unwrap();
        assert_eq!(saved, dir.join("export (1).json"));
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        let saved = save_download(Some(dir.clone()), "gone.json", b"new").unwrap();
        assert_eq!(
            saved,
            dir.join("gone (1).json"),
            "a dangling link counts too"
        );
        assert!(!dir.join("nowhere").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
