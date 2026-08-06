//! Clipboard-image intake for the native TUI (port of
//! `src/server/attachments.ts`).
//!
//! The invariant: image bytes cross the loopback boundary once, are checked
//! before they touch disk, and thereafter messages carry only the durable
//! path. The limits are the MODEL's (5 MB, four formats every provider route
//! accepts), which is why they are enforced here rather than inherited from
//! whatever the terminal handed over.

use std::path::Path;

use serde_json::json;

use bough_core::errors::BoughError;
use bough_core::paths::attachments_dir;

use crate::http::{handler, json as json_res, Handler};

/// The providers' per-image cap.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// media type → stored extension. The four formats every provider accepts.
fn ext_for(media_type: &str) -> Option<&'static str> {
    match media_type {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// The checked write, split from the route so a test exercises it against a
/// temp dir without touching `~/.bough`. Returns the response body JSON.
pub fn store_attachment(
    dir: &Path,
    media_type: &str,
    bytes: &[u8],
) -> Result<serde_json::Value, BoughError> {
    let Some(ext) = ext_for(media_type) else {
        return Err(BoughError::bad_request(
            "unsupported image type: use PNG, JPEG, GIF, or WebP",
        ));
    };
    if bytes.is_empty() {
        return Err(BoughError::bad_request("image is empty"));
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(BoughError::bad_request(
            "image is over the 5 MB limit; downscale or crop it first",
        ));
    }
    let path = dir.join(format!("{}.{ext}", uuid::Uuid::new_v4()));
    let write = std::fs::create_dir_all(dir).and_then(|()| {
        // `wx`-equivalent: refuse to overwrite (the uuid makes a collision
        // absurd, but the flag is the contract).
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, bytes))
    });
    if write.is_err() {
        return Err(BoughError::bad_request("could not save clipboard image"));
    }
    Ok(json!({
        "path": path.to_string_lossy(),
        "mediaType": media_type,
        "name": format!("clipboard.{ext}"),
        "size": bytes.len(),
    }))
}

/// `POST /attachments` — raw image bytes in, durable path out (201).
pub fn upload_attachment() -> Handler {
    handler(|req, _ctx, _params| async move {
        let media_type = req
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or("").trim().to_lowercase())
            .unwrap_or_default();
        let bytes = axum::body::to_bytes(req.into_body(), usize::MAX)
            .await
            .map_err(|e| BoughError::bad_request(format!("could not read image body: {e}")))?;
        let body = store_attachment(&attachments_dir(), &media_type, &bytes)?;
        Ok(json_res(&body, 201))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("bough-attach-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn a_good_image_is_stored_and_answered_with_path_type_name_and_size() {
        let dir = temp_dir();
        let body = store_attachment(&dir, "image/png", b"\x89PNG fake bytes").unwrap();
        assert_eq!(body["mediaType"], "image/png");
        assert_eq!(body["name"], "clipboard.png");
        assert_eq!(body["size"], 15);
        let path = std::path::PathBuf::from(body["path"].as_str().unwrap());
        assert!(path.starts_with(&dir));
        assert_eq!(std::fs::read(&path).unwrap(), b"\x89PNG fake bytes");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn jpeg_stores_with_the_jpg_extension() {
        let dir = temp_dir();
        let body = store_attachment(&dir, "image/jpeg", b"jpg").unwrap();
        assert_eq!(body["name"], "clipboard.jpg");
        assert!(body["path"].as_str().unwrap().ends_with(".jpg"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wrong_type_empty_and_oversize_bodies_are_each_their_own_400() {
        let dir = temp_dir();
        let wrong = store_attachment(&dir, "text/plain", b"hello").unwrap_err();
        assert_eq!(wrong.status(), 400);
        assert_eq!(
            wrong.to_string(),
            "unsupported image type: use PNG, JPEG, GIF, or WebP"
        );

        let empty = store_attachment(&dir, "image/png", b"").unwrap_err();
        assert_eq!(empty.status(), 400);
        assert_eq!(empty.to_string(), "image is empty");

        let big = vec![0u8; MAX_IMAGE_BYTES + 1];
        let oversize = store_attachment(&dir, "image/png", &big).unwrap_err();
        assert_eq!(oversize.status(), 400);
        assert_eq!(
            oversize.to_string(),
            "image is over the 5 MB limit; downscale or crop it first"
        );
        // None of the refusals left a file behind (the dir was never created).
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn the_route_reads_the_content_type_header_and_refuses_non_images() {
        use crate::app::{create_handler, CreateHandlerOptions};
        use crate::http::testutil;
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let req = axum::extract::Request::builder()
            .method("POST")
            .uri("/attachments")
            .header("content-type", "application/json")
            .body(axum::body::Body::from("{}"))
            .unwrap();
        let res = call.call(req).await;
        assert_eq!(res.status(), 400);
        let body = testutil::body_json(res).await;
        assert_eq!(
            body["error"],
            "unsupported image type: use PNG, JPEG, GIF, or WebP"
        );
    }
}
