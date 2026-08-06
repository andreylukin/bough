//! `GET /search`, `POST /search/reindex` (port of `src/server/search.ts`,
//! wave-1 stub).
//!
//! v1-STUB per server.md §8: the FTS layer has not landed, so a well-formed
//! query is answered with the `SearchIndexUnavailableError`-style 503 — a
//! missing index is a 503 about the INDEX, never a 400 about the query. The
//! one 400 kept live is the empty query, with the TS syntax hint verbatim:
//! it is the single most likely way to arrive here and it must teach the
//! query grammar, not report an outage.

use bough_core::errors::{BoughError, ErrorKind};

use crate::http::{handler, Handler};

/// Verbatim TS `NEEDS_A_QUERY` — the hint that teaches the grammar.
const NEEDS_A_QUERY: &str = "search needs a query — GET /search?q=<words>. Bare words are \
    ANDed; quote a phrase as \"like this\"; OR, NOT, NEAR and pref* work too.";

/// The 503 both routes answer until the FTS layer lands (wave 2). The message
/// keeps the TS shape — cause in parentheses, then the reassurance that the
/// transcripts themselves are intact.
fn index_unavailable() -> BoughError {
    BoughError::http(
        503,
        ErrorKind::BadRequest,
        "the search index is unavailable (keyword search is not yet ported in this build). \
         The transcripts themselves are intact — messages are stored in `messages`, and the \
         index is a projection of them.",
    )
}

/// `GET /search?q=&sessionId=&limit=`.
pub fn search() -> Handler {
    handler(|req, _ctx, _params| async move {
        let query = req.uri().query().unwrap_or("");
        let q = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("q="))
            .map(|v| urlish_decode(v))
            .unwrap_or_default();
        if q.trim().is_empty() {
            return Err(BoughError::bad_request(NEEDS_A_QUERY));
        }
        Err::<axum::response::Response, _>(index_unavailable())
    })
}

/// `POST /search/reindex` — the repair path. Honest stub: there is no index
/// to rebuild yet, and claiming `{rebuilt: true}` would be a lie.
pub fn reindex() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Err::<axum::response::Response, _>(index_unavailable())
    })
}

/// Percent-decode enough of a query-string value to see through `%20`/`+`.
/// The empty-vs-nonempty decision is the only consumer in v1.
fn urlish_decode(v: &str) -> String {
    let mut out = Vec::with_capacity(v.len());
    let bytes = v.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let decoded = v
                    .get(i + 1..i + 3)
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                if let Some(b) = decoded {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;

    #[tokio::test]
    async fn an_empty_query_is_a_400_with_the_syntax_hint() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        for path in ["/search", "/search?q=", "/search?q=%20%20"] {
            let res = call.call(testutil::get(path)).await;
            assert_eq!(res.status(), 400, "{path}");
            let body = testutil::body_json(res).await;
            let msg = body["error"].as_str().unwrap();
            assert!(msg.contains("search needs a query"), "{msg}");
            assert!(msg.contains("Bare words are ANDed"), "{msg}");
        }
    }

    #[tokio::test]
    async fn a_real_query_is_a_503_about_the_index_never_a_400_about_the_query() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/search?q=retry+logic")).await;
        assert_eq!(res.status(), 503);
        let body = testutil::body_json(res).await;
        let msg = body["error"].as_str().unwrap();
        assert!(msg.contains("the search index is unavailable"), "{msg}");
        assert!(msg.contains("transcripts themselves are intact"), "{msg}");
    }

    #[tokio::test]
    async fn reindex_is_the_same_503_rather_than_a_fake_rebuilt_receipt() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::req("POST", "/search/reindex", None)).await;
        assert_eq!(res.status(), 503);
    }
}
