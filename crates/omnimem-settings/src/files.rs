//! Files the panel hands over or takes in: a download is an attachment the
//! browser saves where its person chooses, and uploads arrive as multipart
//! forms.

use axum::extract::Multipart;
use axum::http::header;
use axum::response::{IntoResponse, Response};

/// `data` as a download named `filename`. The name goes in quotes with
/// anything that could end them, or the header, replaced, so a skill's name
/// can't add a header or a second filename.
pub(crate) fn attachment(filename: &str, content_type: &'static str, data: Vec<u8>) -> Response {
    let safe: String = filename
        .chars()
        .map(|c| {
            if c == '"' || c == '\\' || c.is_control() || !c.is_ascii() {
                '_'
            } else {
                c
            }
        })
        .collect();
    (
        [
            (header::CONTENT_TYPE, content_type.to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{safe}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        data,
    )
        .into_response()
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
    fn a_download_name_cannot_break_out_of_its_quotes() {
        let response = attachment("rust\"; x=\"y\r\n.zip", "application/zip", b"z".to_vec());
        assert_eq!(
            response.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=\"rust_; x=_y__.zip\""
        );
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/zip");
    }
}
