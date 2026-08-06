//! What ⌘v hands the composer: a picture, or words (port of
//! `src/tui/clipboard.ts` + the pasteboard half of `main.tsx`).
//!
//! THE ORDER IS THE WHOLE POINT. This used to read `pbpaste` first and return
//! its text whenever it was non-empty, which is exactly backwards for the
//! gesture it serves. A macOS pasteboard holding an image almost always ALSO
//! holds text — copying a file in Finder puts its path (or `file://` URL) on as
//! a string, and several apps put a filename beside their image data. So "copy
//! image, ⌘v" put the PATH in the composer and the model was sent a line of
//! prose about a file it could not open. The image data is the more specific
//! offer; it is read first, and text is the fallback rather than the winner.
//!
//! SECOND — a pasteboard whose text IS a path to an image file is a picture
//! too. That is what Finder's Copy gives (no image data at all, just the file),
//! and it is the case the user actually hits. The file is read here, so what
//! crosses to the server is bytes like any other paste; nothing downstream
//! learns a second shape. Anything else — a path to a non-image, a file that is
//! gone, more than one line — stays text, because guessing wrong would swallow
//! a paste the user meant as words.
//!
//! PORT NOTE (row 2.26): the macOS image read keeps the TS tree's
//! compiled-on-first-use Swift helper (`~/.bough/bin/pasteboard-png`, TIFF→PNG)
//! rather than pulling `arboard` in. `arboard` hands back raw RGBA that would
//! need an encoder beside it (a second dependency) to become the PNG the
//! providers accept, and it links AppKit/X11 into a crate whose whole rule is
//! "loopback HTTP and nothing else". Where `swiftc` is absent the helper simply
//! does not build and the pasteboard reads as text — the honest degradation,
//! not a half-wired path.

use std::path::Path;

/// What the pasteboard offered. `None` = nothing usable at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clipboard {
    Image { bytes: Vec<u8>, media_type: String },
    Text(String),
}

/// The four the providers accept, keyed by the extension a pasteboard path
/// carries.
fn media_type_for(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// The image file a clipboard's TEXT names, or `None` if the text is just text.
///
/// Pure and total: it decides from the string alone and never touches disk, so
/// the rule can be pinned by tests without a pasteboard. A `file://` URL that
/// does not parse is text, like every other thing that is not a path we
/// recognise.
pub fn clipboard_image_path(text: &str) -> Option<(String, String)> {
    let one = text.trim();
    if one.is_empty() || one.contains('\n') {
        return None;
    }
    let path = if let Some(rest) = one.strip_prefix("file://") {
        file_url_to_path(rest)?
    } else {
        one.to_string()
    };
    if !Path::new(&path).is_absolute() {
        return None;
    }
    let ext = match path.rfind('.') {
        Some(i) => path[i + 1..].to_string(),
        None => path.clone(),
    };
    media_type_for(&ext).map(|m| (path, m.to_string()))
}

/// The path half of a `file://` URL: an empty or `localhost` authority, then
/// percent-decoding. Anything else (a real host, a bad escape) is not a path.
fn file_url_to_path(rest: &str) -> Option<String> {
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = rest.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Read that file as a paste, or fall back to the text if it cannot be read.
///
/// A missing or unreadable file is NOT an error here: the string may simply
/// have been a path the user meant to type. Handing back the text is what they
/// asked for either way.
pub async fn clipboard_from_text(text: &str) -> Clipboard {
    let Some((path, media_type)) = clipboard_image_path(text) else {
        return Clipboard::Text(text.to_string());
    };
    match tokio::fs::metadata(&path).await {
        Ok(meta) if meta.is_file() => match tokio::fs::read(&path).await {
            Ok(bytes) => Clipboard::Image { bytes, media_type },
            Err(_) => Clipboard::Text(text.to_string()),
        },
        _ => Clipboard::Text(text.to_string()),
    }
}

/// The whole ⌘v read: the pasteboard's image data first, its text second.
/// Non-darwin ⇒ `None` (there is no pasteboard this reaches).
pub async fn paste_clipboard() -> Option<Clipboard> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    if let Some(bytes) = pasteboard_png().await {
        return Some(Clipboard::Image {
            bytes,
            media_type: "image/png".to_string(),
        });
    }
    let out = tokio::process::Command::new("pbpaste")
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.is_empty() {
        return None;
    }
    Some(clipboard_from_text(&text).await)
}

/// `$BOUGH_HOME` (else `~/.bough`) `/bin` — where the helper is compiled.
fn bough_bin_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var("BOUGH_HOME").ok().filter(|h| !h.is_empty());
    let base = match home {
        Some(h) => std::path::PathBuf::from(h),
        None => std::path::PathBuf::from(std::env::var("HOME").ok()?).join(".bough"),
    };
    Some(base.join("bin"))
}

const PASTEBOARD_SWIFT: &str = "import AppKit\nlet p = NSPasteboard.general\nguard let d = p.data(forType: .tiff), let i = NSImage(data: d), let b = i.tiffRepresentation, let r = NSBitmapImageRep(data: b), let png = r.representation(using: .png, properties: [:]) else { exit(1) }\nFileHandle.standardOutput.write(png)\n";

/// The pasteboard's image data as PNG bytes, or `None` if it holds none.
/// Compiles the extractor once; a machine without `swiftc` degrades to "no
/// image on the pasteboard", which is what the text path then answers.
async fn pasteboard_png() -> Option<Vec<u8>> {
    let dir = bough_bin_dir()?;
    let helper = dir.join("pasteboard-png");
    if tokio::fs::metadata(&helper).await.is_err() {
        tokio::fs::create_dir_all(&dir).await.ok()?;
        let source = dir.join("pasteboard-png.swift");
        tokio::fs::write(&source, PASTEBOARD_SWIFT).await.ok()?;
        let status = tokio::process::Command::new("swiftc")
            .arg("-O")
            .arg(&source)
            .arg("-o")
            .arg(&helper)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .ok()?;
        if !status.success() {
            return None;
        }
    }
    let out = tokio::process::Command::new(&helper)
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    (out.status.success() && !out.stdout.is_empty()).then_some(out.stdout)
}

// ---------------------------------------------------------------------------
// Tests — ports of `src/tui/clipboard.test.ts`
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clipboard_naming_an_image_file_is_a_picture_however_it_names_it() {
        assert_eq!(
            clipboard_image_path("/tmp/shot.png"),
            Some(("/tmp/shot.png".into(), "image/png".into()))
        );
        assert_eq!(
            clipboard_image_path("  /tmp/a.JPEG\n"),
            Some(("/tmp/a.JPEG".into(), "image/jpeg".into()))
        );
        assert_eq!(
            clipboard_image_path("file:///tmp/with%20space.webp"),
            Some(("/tmp/with space.webp".into(), "image/webp".into()))
        );
    }

    #[test]
    fn anything_that_is_not_one_absolute_image_path_stays_text() {
        // A relative path, a directory, a non-image, prose that merely mentions
        // one, and a multi-line paste whose first line is a path: all words.
        for text in [
            "",
            "shot.png",
            "/tmp/notes.txt",
            "/tmp/pictures",
            "look at /tmp/shot.png",
            "/tmp/a.png\n/tmp/b.png",
            "file://",
        ] {
            assert_eq!(clipboard_image_path(text), None, "{text:?}");
        }
    }

    #[tokio::test]
    async fn the_named_file_is_read_as_bytes_and_a_missing_one_falls_back_to_the_text() {
        let dir = std::env::temp_dir().join(format!("bough-clip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shot.png");
        std::fs::write(&path, [137u8, 80, 78, 71]).unwrap();
        let path_str = path.to_string_lossy().into_owned();

        match clipboard_from_text(&path_str).await {
            Clipboard::Image { bytes, media_type } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(bytes.len(), 4, "an existing image path attaches");
            }
            other => panic!("expected an image, got {other:?}"),
        }

        let via_url = clipboard_from_text(&format!("file://{path_str}")).await;
        assert!(
            matches!(&via_url, Clipboard::Image { bytes, .. } if bytes.len() == 4),
            "a file:// URL names the same file: {via_url:?}"
        );

        let gone = dir.join("gone.png").to_string_lossy().into_owned();
        assert_eq!(
            clipboard_from_text(&gone).await,
            Clipboard::Text(gone.clone()),
            "a path to nothing is text"
        );
        let as_dir = format!("{}/x.png", dir.to_string_lossy());
        assert_eq!(clipboard_from_text(&as_dir).await, Clipboard::Text(as_dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
