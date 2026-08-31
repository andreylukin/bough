//! Invariant: BOTH halves of the transport speak this module and nothing else. The server (this
//! row) and the client (the launcher's `attach` module) share one frame codec and one version
//! number, so a mismatch is a refused hello with both versions named, never a corrupted screen.
//!
//! The wire is length-prefixed frames over a unix stream: one tag byte, a u32 big-endian payload
//! length, the payload. Client to server: HELLO (JSON), EVENT (a `crossterm::event::Event` as
//! JSON — the `serde` feature is what makes the event types the protocol's vocabulary, so the two
//! halves cannot drift). Server to client: HELLO (JSON), BYTES (raw ANSI, written to the client's
//! stdout verbatim), EXIT (JSON: a code and a reason the client prints after restoring).

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Bumped whenever a frame's meaning changes. A client and server that disagree refuse each other
/// by name rather than guessing.
pub const VERSION: u32 = 1;

/// The one bound on a frame. ANSI diffs are kilobytes; an event is smaller. Anything past this is
/// a corrupted stream, and refusing it beats allocating whatever a bad length field says.
pub const MAX_FRAME: u32 = 4 * 1024 * 1024;

/// Client → server frame tags.
pub const C_HELLO: u8 = 1;
pub const C_EVENT: u8 = 2;

/// Server → client frame tags.
pub const S_HELLO: u8 = 1;
pub const S_BYTES: u8 = 2;
pub const S_EXIT: u8 = 3;

/// The client's first frame: who it is and how big its terminal is.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientHello {
    pub version: u32,
    pub cols: u16,
    pub rows: u16,
}

/// The server's answer: its version, and what the client must mirror on its own terminal.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerHello {
    pub version: u32,
    /// Whether the composition captures the mouse; the client enables capture to match.
    pub mouse: bool,
}

/// The server's last word: the client restores its terminal, prints `reason`, exits `code`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Exit {
    pub code: u8,
    pub reason: String,
}

/// Everything the codec can refuse.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame of {len} bytes exceeds the {MAX_FRAME}-byte bound")]
    TooLarge { len: u32 },
    #[error("bad {what} payload: {detail}")]
    BadPayload { what: &'static str, detail: String },
    #[error("the other side speaks protocol v{theirs}, this side v{ours}")]
    Version { ours: u32, theirs: u32 },
}

/// Write one frame: tag, length, payload.
pub async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    tag: u8,
    payload: &[u8],
) -> Result<(), ProtoError> {
    let len = u32::try_from(payload.len()).map_err(|_| ProtoError::TooLarge { len: u32::MAX })?;
    if len > MAX_FRAME {
        return Err(ProtoError::TooLarge { len });
    }
    w.write_all(&[tag]).await?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(payload).await?;
    w.flush().await?;
    Ok(())
}

/// Read one frame. `Ok(None)` is a clean EOF at a frame boundary; an EOF inside a frame is an
/// error, because half a frame means the stream died mid-sentence.
pub async fn read_frame<R: tokio::io::AsyncRead + Unpin>(
    r: &mut R,
) -> Result<Option<(u8, Vec<u8>)>, ProtoError> {
    let mut tag = [0u8; 1];
    match r.read_exact(&mut tag).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len);
    if len > MAX_FRAME {
        return Err(ProtoError::TooLarge { len });
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).await?;
    Ok(Some((tag[0], payload)))
}

/// Serialize one typed payload as JSON. The codec's only encoding; spelled once.
pub fn encode<T: serde::Serialize>(what: &'static str, value: &T) -> Result<Vec<u8>, ProtoError> {
    serde_json::to_vec(value).map_err(|e| ProtoError::BadPayload {
        what,
        detail: e.to_string(),
    })
}

/// Deserialize one typed payload.
pub fn decode<T: serde::de::DeserializeOwned>(
    what: &'static str,
    payload: &[u8],
) -> Result<T, ProtoError> {
    serde_json::from_slice(payload).map_err(|e| ProtoError::BadPayload {
        what,
        detail: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_frame_round_trips_tag_and_payload() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, C_EVENT, b"hello").await.expect("write");
        let (tag, payload) = read_frame(&mut b).await.expect("read").expect("a frame");
        assert_eq!(tag, C_EVENT);
        assert_eq!(payload, b"hello");
    }

    #[tokio::test]
    async fn a_clean_eof_is_none_and_a_torn_frame_is_an_error() {
        let (a, mut b) = tokio::io::duplex(1024);
        drop(a);
        assert!(read_frame(&mut b).await.expect("clean eof").is_none());

        let (mut a, mut b) = tokio::io::duplex(1024);
        use tokio::io::AsyncWriteExt;
        // A tag and a length promising 100 bytes, then the stream dies.
        a.write_all(&[S_BYTES]).await.unwrap();
        a.write_all(&100u32.to_be_bytes()).await.unwrap();
        a.write_all(b"short").await.unwrap();
        drop(a);
        assert!(
            read_frame(&mut b).await.is_err(),
            "half a frame is an error"
        );
    }

    #[tokio::test]
    async fn a_length_past_the_bound_is_refused_without_allocating_it() {
        let (mut a, mut b) = tokio::io::duplex(64);
        use tokio::io::AsyncWriteExt;
        a.write_all(&[S_BYTES]).await.unwrap();
        a.write_all(&(MAX_FRAME + 1).to_be_bytes()).await.unwrap();
        let err = read_frame(&mut b).await.expect_err("refused");
        assert!(matches!(err, ProtoError::TooLarge { .. }), "{err}");
    }

    #[test]
    fn a_crossterm_event_survives_the_json_encoding() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let ev = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        let bytes = encode("event", &ev).expect("encode");
        let back: Event = decode("event", &bytes).expect("decode");
        assert_eq!(back, ev);
    }

    #[test]
    fn the_hellos_round_trip() {
        let c = ClientHello {
            version: VERSION,
            cols: 120,
            rows: 40,
        };
        let back: ClientHello = decode("hello", &encode("hello", &c).unwrap()).unwrap();
        assert_eq!(back, c);
        let s = ServerHello {
            version: VERSION,
            mouse: true,
        };
        let back: ServerHello = decode("hello", &encode("hello", &s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
