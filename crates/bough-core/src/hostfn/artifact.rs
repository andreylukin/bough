//! `artifact(name, content)` — how a program hands the user something to LOOK
//! at in a browser — and the per-session store underneath it (port of
//! `src/hostfn/artifact.ts`).
//!
//! TWO INVARIANTS.
//!
//! **1. Publishing never touches the workspace.** The bytes go to
//! `~/.bough/artifacts/<sessionId>/`, outside the checkout, so the diff the
//! user reviews stays the work and nothing else. A program that wanted this
//! effect without the verb would `write("report.html", …)` into the repo and
//! drop a generated page into `git status` — exactly the pollution the store
//! exists to prevent.
//!
//! **2. Names and session ids are CONFINED to their directory.** Both arrive
//! from outside — the name from a program's call, the session id from a URL
//! someone can type — and both are used to build a path the *server* then
//! reads or writes. To be plain about what that is: not a sandbox (programs
//! run as the user); confinement guards the server's own path construction,
//! so `GET /artifacts/<id>/<name>` cannot be steered into `~/.ssh` and one
//! session's publish cannot land in another's listing.
//!
//! **The filesystem is the source of truth.** There is no artifacts table and
//! no row to keep in sync: [`list_artifacts`] walks the directory, so a
//! listing survives a database reset, a fresh `bough.db`, or a server that
//! has never seen the session. The HISTORY works the same way — one file per
//! superseded version under `~/.bough/artifact-versions/`, named for when it
//! was published, so [`list_versions`] is a directory read too.
//!
//! **Nothing is ever destroyed.** A republish keeps what it overwrote, and a
//! restore is itself a publish ([`restore_version`]), so there is no cursor to
//! strand and no version that going back makes unreachable.
//!
//! WHY THE STORE LIVES HERE and not in the server crate: `hostfn` may not
//! reference `bough-server`, and the confinement rules must exist exactly
//! once — a store and a server that disagree about which names are legal is a
//! traversal bug waiting for one of them to be updated alone. So the
//! primitives are here, taking plain parameters and no HTTP, and
//! `bough-server::artifacts` imports them to serve and to list over the wire.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::errors::{BoughError, ErrorKind};
use crate::paths::{artifact_versions_dir, artifacts_dir, confine};
use crate::types::{HostFn, TurnCtx};

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// One published file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    /// Session-relative path, forward-slashed (`index.html`, `assets/app.js`).
    pub name: String,
    /// Same-origin path the UI links to: `/artifacts/<sessionId>/<name>`.
    pub url: String,
    /// Absolute loopback URL — what the agent prints for the user to click.
    pub href: String,
    pub bytes: u64,
    /// Publish/update time (mtime epoch ms).
    pub ts: i64,
}

/// Where the store lives and what its links look like.
///
/// Injected rather than read from the environment at each call site: a test
/// points `root` at a temp directory and gets a hermetic store, with no
/// `BOUGH_HOME` mutation and nothing written under the real `~/.bough`.
#[derive(Clone, Default)]
pub struct ArtifactStoreOptions {
    /// Where superseded versions go. Absent = `~/.bough/artifact-versions`.
    pub versions_root: Option<PathBuf>,
    /// The artifacts root. Absent = `~/.bough/artifacts` (`paths`).
    pub root: Option<PathBuf>,
    /// Origin for `href`. Absent = the loopback base this server is reachable at.
    pub base_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Paths — the confinement rules
// ---------------------------------------------------------------------------

/// The loopback base URL this server is reachable at.
///
/// Always 127.0.0.1: the server binds loopback and only loopback, and `href`
/// is what the LOCAL user clicks. The UI links the relative `url` and is
/// origin-agnostic either way.
pub fn server_base_url() -> String {
    let port = std::env::var("BOUGH_PORT").ok().filter(|p| !p.is_empty());
    format!("http://127.0.0.1:{}", port.as_deref().unwrap_or("4321"))
}

fn path_error(message: impl Into<String>) -> BoughError {
    BoughError::http(400, ErrorKind::Path, message)
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
}

/// One session's artifact directory, or a `PathError` when the id is not a
/// single confined segment.
///
/// `confine` alone rejects `..` and absolute ids; the parent check is what
/// rejects a *descending* id like `other/nested`, which stays inside the root
/// but addresses a directory that is not its own — and would let two different
/// id strings name the same directory.
pub fn session_artifact_dir(
    session_id: &str,
    opts: &ArtifactStoreOptions,
) -> Result<PathBuf, BoughError> {
    let raw_root = opts.root.clone().unwrap_or_else(artifacts_dir);
    // Normalized-absolute form of the root itself (candidate "" = the root),
    // so the direct-child comparison below compares like with like.
    let root = confine(&raw_root, Path::new(""))?;
    if session_id.is_empty() {
        return Err(path_error(
            "artifact session id is empty — name the session that published it.",
        ));
    }
    let dir = confine(&root, Path::new(session_id))?;
    if dir == root || dir.parent() != Some(root.as_path()) {
        return Err(path_error(format!(
            "artifact session id must be one path segment: {} resolves to {}, which is not a \
             direct child of {}.",
            json_str(session_id),
            dir.display(),
            root.display(),
        )));
    }
    Ok(dir)
}

/// Resolve `name` under the session's directory. Fails with `PathError` on
/// anything that escapes, and on a name that resolves to the directory itself.
///
/// Leading slashes are stripped rather than rejected: `/index.html` from a URL
/// path, or from a program that wrote an absolute-looking name, means the
/// store's own root, and reading it that way is what every caller intends.
/// Everything after that is confined for real.
pub fn resolve_artifact_path(
    session_id: &str,
    name: &str,
    opts: &ArtifactStoreOptions,
) -> Result<PathBuf, BoughError> {
    let dir = session_artifact_dir(session_id, opts)?;
    let rel = name.trim_start_matches('/');
    if rel.is_empty() {
        return Err(path_error(
            "artifact name is empty — publish under a plain relative name like index.html.",
        ));
    }
    let full = confine(&dir, Path::new(rel))?;
    if full == dir {
        return Err(path_error(format!(
            "artifact name {} names the session's directory, not a file.",
            json_str(name),
        )));
    }
    Ok(full)
}

/// `encodeURIComponent`, byte for byte — the URL the store hands out must
/// round-trip through the server's per-segment decode.
fn encode_uri_component(s: &str) -> String {
    const KEEP: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.!~*'()";
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        if KEEP.contains(b) {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn to_artifact(
    session_id: &str,
    name: &str,
    bytes: u64,
    ts: i64,
    opts: &ArtifactStoreOptions,
) -> Artifact {
    let url = format!(
        "/artifacts/{}/{}",
        encode_uri_component(session_id),
        name.split('/')
            .map(encode_uri_component)
            .collect::<Vec<_>>()
            .join("/"),
    );
    let href = format!(
        "{}{url}",
        opts.base_url.clone().unwrap_or_else(server_base_url)
    );
    Artifact {
        name: name.to_string(),
        url,
        href,
        bytes,
        ts,
    }
}

fn mtime_ms(meta: &std::fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

// ---------------------------------------------------------------------------
// Publish and list
// ---------------------------------------------------------------------------

/// Write `content` into the session's store and describe it.
///
/// Creates parent directories and overwrites an existing artifact of the same
/// name — republishing `index.html` is how a program iterates on a page, and a
/// link the user already has open has to keep working.
///
/// What it overwrote is KEPT: the previous bytes are copied into the version
/// store first, so every republish is recoverable. See [`keep_version`].
pub fn publish_artifact(
    session_id: &str,
    name: &str,
    content: &str,
    opts: &ArtifactStoreOptions,
) -> Result<Artifact, BoughError> {
    let rel: String = name.trim_start_matches('/').to_string();
    let full = resolve_artifact_path(session_id, &rel, opts)?;
    let io = |err: std::io::Error| BoughError::http(500, ErrorKind::Artifact, err.to_string());
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    let kept = keep_version(session_id, &rel, &full, content, opts);
    std::fs::write(&full, content).map_err(io)?;
    let meta = std::fs::metadata(&full).map_err(io)?;
    let ts = mtime_ms(&meta).unwrap_or_else(|| crate::types::system_clock()());
    // Two publishes inside one millisecond would give the snapshot and the
    // live file the same id, and an id that names two versions is worse than
    // a timestamp that is one millisecond early.
    if let Some(path) = kept {
        if path.file_name().and_then(|n| n.to_str()) == Some(&ts.to_string()) {
            let mut older = ts - 1;
            while path.with_file_name(older.to_string()).exists() {
                older -= 1;
            }
            let _ = std::fs::rename(&path, path.with_file_name(older.to_string()));
        }
    }
    Ok(to_artifact(session_id, &rel, meta.len(), ts, opts))
}

/// Every artifact a session has published, newest first. An absent directory
/// is an empty list, not an error — a session that never published one is the
/// normal case; an unaddressable id has published nothing, by construction.
///
/// This walks the FILESYSTEM and consults no table, which is the
/// source-of-truth rule made operational: drop the database, start a fresh
/// one, and the listing is still right.
pub fn list_artifacts(session_id: &str, opts: &ArtifactStoreOptions) -> Vec<Artifact> {
    let Ok(dir) = session_artifact_dir(session_id, opts) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk(&dir, "", session_id, opts, &mut out);
    out.sort_by_key(|a| std::cmp::Reverse(a.ts));
    out
}

fn walk(
    abs: &Path,
    rel: &str,
    session_id: &str,
    opts: &ArtifactStoreOptions,
    out: &mut Vec<Artifact>,
) {
    let Ok(entries) = std::fs::read_dir(abs) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let child_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            walk(&abs.join(&name), &child_rel, session_id, opts, out);
            continue;
        }
        if !file_type.is_file() {
            continue; // symlinks and specials are not artifacts
        }
        let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue; // raced a delete
        };
        out.push(to_artifact(
            session_id,
            &child_rel,
            meta.len(),
            mtime_ms(&meta).unwrap_or(0),
            opts,
        ));
    }
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// One point in an artifact's history.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactVersion {
    /// 1-based, oldest first — what the version bar counts in.
    pub version: u32,
    /// When these bytes were published (epoch ms). The stable id: every verb
    /// that names a version names this.
    pub ts: i64,
    pub bytes: u64,
    /// Is this what `GET /artifacts/<id>/<name>` serves right now?
    pub current: bool,
    /// Same-origin path that serves THESE bytes: `?v=<ts>` on the artifact's
    /// own URL, so a version link is the artifact link plus one parameter.
    pub url: String,
}

/// Where one artifact's superseded versions live.
///
/// The artifact's own name becomes a DIRECTORY here, so `assets/app.js` keeps
/// its shape (`<session>/assets/app.js/<ts>`) and two artifacts can never
/// share a history. Confined exactly like the live path, against the versions
/// root instead of the artifacts root.
fn version_dir(
    session_id: &str,
    name: &str,
    opts: &ArtifactStoreOptions,
) -> Result<PathBuf, BoughError> {
    let raw_root = opts
        .versions_root
        .clone()
        .unwrap_or_else(artifact_versions_dir);
    let root = confine(&raw_root, Path::new(""))?;
    let session = confine(&root, Path::new(session_id))?;
    if session == root || session.parent() != Some(root.as_path()) {
        return Err(path_error(format!(
            "artifact session id must be one path segment: {}.",
            json_str(session_id),
        )));
    }
    let rel = name.trim_start_matches('/');
    if rel.is_empty() {
        return Err(path_error("artifact name is empty."));
    }
    let dir = confine(&session, Path::new(rel))?;
    if dir == session {
        return Err(path_error(format!(
            "artifact name {} names the session's directory, not a file.",
            json_str(name),
        )));
    }
    Ok(dir)
}

/// Copy what is about to be overwritten into the version store.
///
/// BEST EFFORT, DELIBERATELY. A publish that succeeds must not be reported as
/// a failure because the history could not be written — the artifact is the
/// product and the history is the convenience. A lost snapshot costs one
/// undo, which is the same thing that happens on a machine that has never had
/// the version directory writable.
///
/// AN IDENTICAL REPUBLISH IS NOT A VERSION. Programs re-publish unchanged
/// pages constantly (a turn that regenerates a report from the same data), and
/// a history of forty identical entries is a history nobody can navigate.
fn keep_version(
    session_id: &str,
    name: &str,
    live: &Path,
    incoming: &str,
    opts: &ArtifactStoreOptions,
) -> Option<PathBuf> {
    let previous = std::fs::read(live).ok()?; // absent = first publish
    if previous == incoming.as_bytes() {
        return None;
    }
    let meta = std::fs::metadata(live).ok()?;
    let ts = mtime_ms(&meta).unwrap_or_else(|| crate::types::system_clock()());
    let dir = version_dir(session_id, name, opts).ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(ts.to_string());
    std::fs::write(&path, &previous).ok()?;
    Some(path)
}

/// An artifact's history, OLDEST FIRST, with the live file last.
///
/// The live file is part of the history rather than something beside it —
/// "v5 of 5" has to be a place you can be, or stepping back and forth has an
/// off-by-one at one end. Empty when the artifact does not exist.
pub fn list_versions(
    session_id: &str,
    name: &str,
    opts: &ArtifactStoreOptions,
) -> Vec<ArtifactVersion> {
    let Ok(live) = resolve_artifact_path(session_id, name, opts) else {
        return Vec::new();
    };
    let Ok(live_meta) = std::fs::metadata(&live) else {
        return Vec::new();
    };
    let rel = name.trim_start_matches('/');
    let mut out: Vec<(i64, u64)> = Vec::new();
    if let Ok(dir) = version_dir(session_id, rel, opts) {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let Some(ts) = entry
                    .file_name()
                    .to_str()
                    .and_then(|n| n.parse::<i64>().ok())
                else {
                    continue; // not one of ours
                };
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if meta.is_file() {
                    out.push((ts, meta.len()));
                }
            }
        }
    }
    let live_ts = mtime_ms(&live_meta).unwrap_or(0);
    // `publish_artifact` moves a snapshot off a colliding id, so a snapshot
    // sharing the live timestamp here means the live file was written by
    // something else entirely (an edited store, a restored backup). Dropping
    // it keeps every id naming exactly one version.
    out.retain(|(ts, _)| *ts != live_ts);
    out.push((live_ts, live_meta.len()));
    out.sort_by_key(|(ts, _)| *ts);
    let last = out.len();
    out.into_iter()
        .enumerate()
        .map(|(i, (ts, bytes))| ArtifactVersion {
            version: i as u32 + 1,
            ts,
            bytes,
            current: i + 1 == last,
            url: format!(
                "{}?v={ts}",
                to_artifact(session_id, rel, bytes, ts, opts).url
            ),
        })
        .collect()
}

/// The bytes of one version, live file included. `None` when no version of
/// this artifact carries that timestamp.
pub fn read_version(
    session_id: &str,
    name: &str,
    ts: i64,
    opts: &ArtifactStoreOptions,
) -> Option<Vec<u8>> {
    let rel = name.trim_start_matches('/');
    let live = resolve_artifact_path(session_id, rel, opts).ok()?;
    if std::fs::metadata(&live).ok().and_then(|m| mtime_ms(&m)) == Some(ts) {
        return std::fs::read(&live).ok();
    }
    let dir = version_dir(session_id, rel, opts).ok()?;
    // `ts.to_string()` is the whole filename, so a caller-supplied value can
    // name a file in this directory and nowhere else — but confine anyway,
    // because "an integer cannot traverse" is a property of the parse, and
    // the parse is not in this function.
    let path = confine(&dir, Path::new(&ts.to_string())).ok()?;
    std::fs::read(path).ok()
}

/// Bring an old version back, by PUBLISHING it again.
///
/// Restoring is an ordinary publish, which is what makes going back and
/// forward symmetric: v2 restored becomes v6, v5 is still there to restore
/// next, and a restore someone regrets is undone the same way it was made.
/// Nothing in this store is ever destroyed, so there is no rewound state for a
/// later publish to invalidate — the lost-redo problem does not arise because
/// there is no cursor to strand.
pub fn restore_version(
    session_id: &str,
    name: &str,
    ts: i64,
    opts: &ArtifactStoreOptions,
) -> Result<Artifact, BoughError> {
    let Some(bytes) = read_version(session_id, name, ts, opts) else {
        return Err(BoughError::http(
            404,
            ErrorKind::Artifact,
            format!(
                "no version of {} published at {ts}. `GET /sessions/<id>/artifacts/versions` \
                 lists the timestamps that exist.",
                json_str(name),
            ),
        ));
    };
    let content = String::from_utf8(bytes).map_err(|_| {
        BoughError::http(
            400,
            ErrorKind::Artifact,
            format!(
                "version {ts} of {} is not text and cannot be restored through this verb.",
                json_str(name),
            ),
        )
    })?;
    publish_artifact(session_id, name, &content, opts)
}

// ---------------------------------------------------------------------------
// The host function
// ---------------------------------------------------------------------------

/// Publish with the failure text a MODEL reads.
///
/// `confine`'s message explains a path escape to a developer; this one tells
/// the next round what to do instead, which is what error text is for. A
/// refusal costs a `catch`, not a round.
pub fn publish_for_program(
    session_id: &str,
    name: &str,
    content: &str,
    opts: &ArtifactStoreOptions,
) -> Result<Artifact, BoughError> {
    publish_artifact(session_id, name, content, opts).map_err(|err| {
        if err.name() == "PathError" {
            BoughError::http(
                400,
                ErrorKind::Artifact,
                format!(
                    "artifact(\"{name}\"): that name escapes this session's artifact directory, \
                     and nothing was written. Publish under a plain relative name — \
                     \"index.html\", \"assets/app.js\" — with no leading slash and no \"..\" \
                     segments.",
                ),
            )
        } else {
            BoughError::http(
                500,
                ErrorKind::Artifact,
                format!(
                    "artifact(\"{name}\"): could not be written ({err}). Check that the name is \
                     a usable filename, then publish again.",
                ),
            )
        }
    })
}

/// Build the bridged `artifact` host function for one turn.
///
/// Scoped to `ctx.session_id`, which is the confinement that matters at this
/// layer: a program cannot name another session's store because it never gets
/// to name a session at all. A subagent therefore publishes into its OWN
/// directory and its `href` still works — the store is per-session, not
/// per-tree, and the report it hands back carries the link.
///
/// The wire is string-only, so the result travels as JSON and the worker
/// re-inflates it before the program sees it.
pub fn create_artifact_host_fn(ctx: &TurnCtx, deps: ArtifactStoreOptions) -> HostFn {
    let session_id = ctx.session_id.clone();
    Arc::new(move |args: Vec<String>| {
        let session_id = session_id.clone();
        let deps = deps.clone();
        let name = args.first().cloned().unwrap_or_default();
        let content = args.get(1).cloned().unwrap_or_default();
        Box::pin(async move {
            let published = publish_for_program(&session_id, &name, &content, &deps)?;
            // NOTE (kept from the TS port): `name` and `bytes` ride along with
            // `{url, href}` because a program publishing several files wants
            // to log what it wrote, and a zero-byte artifact is otherwise
            // indistinguishable from a written one.
            Ok(serde_json::json!({
                "name": published.name,
                "url": published.url,
                "href": published.href,
                "bytes": published.bytes,
            })
            .to_string())
        })
    })
}

// ---------------------------------------------------------------------------
// Tests — port of src/hostfn/artifact.test.ts plus the store half of
// src/server/artifacts.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new() -> TmpDir {
            let dir = std::env::temp_dir()
                .join(format!("bough-hostfn-artifact-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            TmpDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn store(root: &Path) -> ArtifactStoreOptions {
        ArtifactStoreOptions {
            root: Some(root.join("live")),
            // A SIBLING of the artifacts root, never inside it — the same
            // relationship the real directories have, so a test would catch
            // a history that started showing up in listings.
            versions_root: Some(root.join("versions")),
            base_url: Some("http://127.0.0.1:4321".into()),
        }
    }

    /// A fabricated turn context — no server, no database reads on this path.
    fn turn_ctx(session_id: &str) -> TurnCtx {
        use std::sync::{Mutex, RwLock};
        let db: crate::types::SharedDb = Arc::new(Mutex::new(
            crate::db::sqlite_db::SqliteDb::new(":memory:", Default::default()).unwrap(),
        ));
        let app = crate::types::AppCtx {
            db,
            bus: Arc::new(crate::bus::Bus::new(crate::types::system_clock())),
            llm: None,
            model: Some("test-model".into()),
            effort: None,
            now: crate::types::system_clock(),
            cheap: None,
            host: Arc::new(crate::types::HostState::new()),
            starter: Arc::new(RwLock::new(None)),
            turn_registry: Arc::new(crate::turn::queue::TurnRegistry::new()),
            model_defaults_path: None,
        };
        TurnCtx {
            app,
            session_id: session_id.into(),
            turn_id: "t1".into(),
            message_id: "m1".into(),
            workspace: "/".into(),
            model: "test-model".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
            exits: Arc::new(Mutex::new(Vec::new())),
            record: None,
            reads: Arc::new(Mutex::new(Vec::new())),
            touched: Arc::new(Mutex::new(Vec::new())),
            round_refs: Arc::new(Mutex::new(Vec::new())),
            mcp_grant: None,
            depth: 0,
        }
    }

    /// Read a published file. `root` is the TmpDir; the live store is the
    /// `live/` sibling of the version store under it (see [`store`]).
    fn read(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(root.join("live").join(rel)).unwrap()
    }

    fn names(session_id: &str, opts: &ArtifactStoreOptions) -> Vec<String> {
        list_artifacts(session_id, opts)
            .into_iter()
            .map(|a| a.name)
            .collect()
    }

    // ---- history -------------------------------------------------------------

    /// Publish, then step the file's mtime back so the next publish lands in a
    /// distinguishable millisecond. Real publishes are separated by LLM round
    /// trips; a test's are not, and the ids are mtimes.
    fn publish_at(
        session_id: &str,
        name: &str,
        content: &str,
        ts: i64,
        opts: &ArtifactStoreOptions,
    ) -> Artifact {
        publish_artifact(session_id, name, content, opts).unwrap();
        let path = resolve_artifact_path(session_id, name, opts).unwrap();
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_millis(ts as u64);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        to_artifact(session_id, name, meta.len(), ts, opts)
    }

    #[test]
    fn every_republish_keeps_what_it_overwrote_oldest_first_with_the_live_file_last() {
        let tmp = TmpDir::new();
        let opts = store(tmp.path());
        publish_at("s1", "report.html", "v1", 1_000, &opts);
        publish_at("s1", "report.html", "v2", 2_000, &opts);
        publish_at("s1", "report.html", "v3", 3_000, &opts);

        let versions = list_versions("s1", "report.html", &opts);
        assert_eq!(versions.len(), 3, "{versions:?}");
        assert_eq!(
            versions.iter().map(|v| v.version).collect::<Vec<_>>(),
            [1, 2, 3],
            "oldest first, numbered from one"
        );
        assert_eq!(
            versions.iter().map(|v| v.current).collect::<Vec<_>>(),
            [false, false, true],
            "the live file is the newest version, not something beside them"
        );
        assert_eq!(
            (0..3)
                .map(|i| read_version("s1", "report.html", versions[i].ts, &opts).unwrap())
                .collect::<Vec<_>>(),
            [b"v1".to_vec(), b"v2".to_vec(), b"v3".to_vec()],
        );
        // The version URL is the artifact's own URL plus one parameter, so a
        // link to a version is a link.
        assert_eq!(
            versions[0].url,
            format!("/artifacts/s1/report.html?v={}", versions[0].ts)
        );
    }

    #[test]
    fn an_identical_republish_is_not_a_new_version() {
        let tmp = TmpDir::new();
        let opts = store(tmp.path());
        publish_at("s1", "page.html", "same", 1_000, &opts);
        publish_at("s1", "page.html", "same", 2_000, &opts);
        publish_at("s1", "page.html", "same", 3_000, &opts);
        assert_eq!(
            list_versions("s1", "page.html", &opts).len(),
            1,
            "a history of identical entries is one nobody can navigate"
        );
    }

    #[test]
    fn restoring_appends_so_back_and_forward_both_stay_possible() {
        let tmp = TmpDir::new();
        let opts = store(tmp.path());
        publish_at("s1", "p.html", "v1", 1_000, &opts);
        publish_at("s1", "p.html", "v2", 2_000, &opts);
        let v3 = publish_at("s1", "p.html", "v3", 3_000, &opts);

        let versions = list_versions("s1", "p.html", &opts);
        restore_version("s1", "p.html", versions[0].ts, &opts).unwrap();
        assert_eq!(read(tmp.path(), "s1/p.html"), "v1", "the live file rewound");

        let after = list_versions("s1", "p.html", &opts);
        assert_eq!(after.len(), 4, "restoring is a publish: {after:?}");
        assert!(after.last().unwrap().current);
        assert_eq!(
            read_version("s1", "p.html", after[3].ts, &opts).unwrap(),
            b"v1".to_vec()
        );
        // FORWARD: what was current before the restore is still there to
        // restore in turn, which is the whole reason restoring appends.
        assert_eq!(
            read_version("s1", "p.html", v3.ts, &opts).unwrap(),
            b"v3".to_vec()
        );
        restore_version("s1", "p.html", v3.ts, &opts).unwrap();
        assert_eq!(read(tmp.path(), "s1/p.html"), "v3");
        assert_eq!(list_versions("s1", "p.html", &opts).len(), 5);
    }

    #[test]
    fn restoring_a_timestamp_that_names_no_version_is_a_404_that_says_where_to_look() {
        let tmp = TmpDir::new();
        let opts = store(tmp.path());
        publish_at("s1", "p.html", "v1", 1_000, &opts);
        let err = restore_version("s1", "p.html", 424_242, &opts).unwrap_err();
        assert_eq!(err.status(), 404);
        assert!(err.to_string().contains("versions"), "{err}");
        assert_eq!(read(tmp.path(), "s1/p.html"), "v1", "and nothing changed");
    }

    #[test]
    fn the_history_lives_outside_the_artifact_store_so_it_is_never_listed_or_served() {
        let tmp = TmpDir::new();
        let opts = store(tmp.path());
        publish_at("s1", "p.html", "v1", 1_000, &opts);
        publish_at("s1", "p.html", "v2", 2_000, &opts);
        assert_eq!(
            names("s1", &opts),
            vec!["p.html"],
            "the history must not appear in the listing"
        );
        // And it cannot be reached through the artifact path at all: the
        // version store is not under the artifacts root.
        let live_root = tmp.path().join("live");
        assert!(!tmp.path().join("versions").starts_with(&live_root));
        assert!(list_versions("s1", "p.html", &opts).len() == 2);
    }

    #[test]
    fn one_sessions_history_cannot_be_reached_through_anothers_name() {
        let tmp = TmpDir::new();
        let opts = store(tmp.path());
        publish_at("victim", "secret.html", "v1", 1_000, &opts);
        publish_at("victim", "secret.html", "v2", 2_000, &opts);
        let theirs = list_versions("victim", "secret.html", &opts);
        assert_eq!(theirs.len(), 2);
        // The traversal a URL can express, against the version verbs.
        assert!(list_versions("attacker", "../victim/secret.html", &opts).is_empty());
        assert!(read_version("attacker", "../victim/secret.html", theirs[0].ts, &opts).is_none());
        assert!(restore_version("attacker", "../victim/secret.html", theirs[0].ts, &opts).is_err());
    }

    #[test]
    fn an_artifact_with_no_history_yet_is_one_version_and_a_missing_one_is_none() {
        let tmp = TmpDir::new();
        let opts = store(tmp.path());
        publish_at("s1", "only.html", "v1", 1_000, &opts);
        let versions = list_versions("s1", "only.html", &opts);
        assert_eq!(versions.len(), 1);
        assert!(versions[0].current, "the only version is the current one");
        assert!(list_versions("s1", "never-published.html", &opts).is_empty());
    }

    // ---- the bridged host function ------------------------------------------

    #[tokio::test]
    async fn artifact_writes_into_the_sessions_store_and_returns_url_and_href_as_json() {
        let tmp = TmpDir::new();
        let artifact = create_artifact_host_fn(&turn_ctx("sX"), store(tmp.path()));
        let result: serde_json::Value = serde_json::from_str(
            &artifact(vec!["index.html".into(), "<h1>report</h1>".into()])
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "name": "index.html",
                "url": "/artifacts/sX/index.html",
                "href": "http://127.0.0.1:4321/artifacts/sX/index.html",
                "bytes": "<h1>report</h1>".len(),
            })
        );
        assert_eq!(read(tmp.path(), "sX/index.html"), "<h1>report</h1>");
    }

    #[tokio::test]
    async fn artifact_is_scoped_to_its_own_session_it_cannot_name_anothers() {
        let tmp = TmpDir::new();
        let spawner = create_artifact_host_fn(&turn_ctx("spawner"), store(tmp.path()));
        let child = create_artifact_host_fn(&turn_ctx("child"), store(tmp.path()));
        spawner(vec!["a.html".into(), "spawner".into()])
            .await
            .unwrap();
        child(vec!["a.html".into(), "child".into()]).await.unwrap();

        assert_eq!(read(tmp.path(), "spawner/a.html"), "spawner");
        assert_eq!(read(tmp.path(), "child/a.html"), "child");
        assert_eq!(names("child", &store(tmp.path())), vec!["a.html"]);

        // Reaching sideways is a path escape, not a write into the sibling's store.
        assert!(child(vec!["../spawner/a.html".into(), "pwned".into()])
            .await
            .is_err());
        assert_eq!(read(tmp.path(), "spawner/a.html"), "spawner");
    }

    #[tokio::test]
    async fn an_escaping_name_is_refused_with_text_naming_the_move_and_writes_nothing() {
        let tmp = TmpDir::new();
        let artifact = create_artifact_host_fn(&turn_ctx("sY"), store(tmp.path()));
        for bad in ["../escape.html", "sub/../../escape.html", ""] {
            let err = artifact(vec![bad.into(), "pwned".into()])
                .await
                .expect_err(&format!("expected a refusal for {bad:?}"));
            assert_eq!(err.name(), "ArtifactError");
            assert_eq!(err.status(), 400);
            let message = err.to_string();
            assert!(
                message.contains("escapes this session's artifact directory"),
                "{message}"
            );
            assert!(message.contains("plain relative name"), "{message}");
            assert!(message.contains("index.html"), "{message}");
            assert!(message.contains("nothing was written"), "{message}");
        }
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn republishing_overwrites_in_place_so_an_open_link_keeps_working() {
        let tmp = TmpDir::new();
        let artifact = create_artifact_host_fn(&turn_ctx("sZ"), store(tmp.path()));
        let first: serde_json::Value = serde_json::from_str(
            &artifact(vec!["page.html".into(), "v1".into()])
                .await
                .unwrap(),
        )
        .unwrap();
        let second: serde_json::Value = serde_json::from_str(
            &artifact(vec!["page.html".into(), "v2-longer".into()])
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(second["url"], first["url"]);
        assert_eq!(second["bytes"], serde_json::json!("v2-longer".len()));
        assert_eq!(names("sZ", &store(tmp.path())), vec!["page.html"]);
    }

    #[tokio::test]
    async fn nested_asset_paths_publish_and_list_with_forward_slashes() {
        let tmp = TmpDir::new();
        let artifact = create_artifact_host_fn(&turn_ctx("sN"), store(tmp.path()));
        artifact(vec!["index.html".into(), "<html></html>".into()])
            .await
            .unwrap();
        let asset: serde_json::Value = serde_json::from_str(
            &artifact(vec!["assets/app.js".into(), "console.log(1)".into()])
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(asset["name"], "assets/app.js");
        assert_eq!(asset["url"], "/artifacts/sN/assets/app.js");
        let mut listed = names("sN", &store(tmp.path()));
        listed.sort();
        assert_eq!(listed, vec!["assets/app.js", "index.html"]);
    }

    // ---- the store (the non-HTTP half of server/artifacts.test.ts) ----------

    #[test]
    fn publish_artifact_writes_under_the_session_dir_and_returns_url_and_href() {
        let tmp = TmpDir::new();
        let art =
            publish_artifact("sessAbc", "index.html", "<h1>hi</h1>", &store(tmp.path())).unwrap();
        assert_eq!(art.name, "index.html");
        assert_eq!(art.url, "/artifacts/sessAbc/index.html");
        assert_eq!(
            art.href,
            "http://127.0.0.1:4321/artifacts/sessAbc/index.html"
        );
        assert_eq!(art.bytes, "<h1>hi</h1>".len() as u64);
        assert_eq!(read(tmp.path(), "sessAbc/index.html"), "<h1>hi</h1>");
    }

    #[test]
    fn publish_artifact_creates_nested_paths_and_overwrites_in_place() {
        let tmp = TmpDir::new();
        publish_artifact("s1", "assets/app.js", "v1", &store(tmp.path())).unwrap();
        let two = publish_artifact("s1", "assets/app.js", "v2-longer", &store(tmp.path())).unwrap();
        assert_eq!(two.name, "assets/app.js");
        assert_eq!(read(tmp.path(), "s1/assets/app.js"), "v2-longer");
        // Republishing must not leave two files behind; the link stays valid.
        assert_eq!(names("s1", &store(tmp.path())), vec!["assets/app.js"]);
    }

    #[test]
    fn a_leading_slash_means_the_store_root_not_the_filesystem_root() {
        let tmp = TmpDir::new();
        let art = publish_artifact("s1", "/index.html", "x", &store(tmp.path())).unwrap();
        assert_eq!(art.name, "index.html");
        assert_eq!(read(tmp.path(), "s1/index.html"), "x");

        // An absolute-LOOKING name is not a traversal — the leading slash
        // means the store's own root, so it lands inside the session dir
        // rather than at /etc.
        let passwd =
            publish_artifact("s1", "/etc/passwd", "not the real one", &store(tmp.path())).unwrap();
        assert_eq!(passwd.name, "etc/passwd");
        assert_eq!(read(tmp.path(), "s1/etc/passwd"), "not the real one");
    }

    #[test]
    fn ac_traversal_in_the_name_is_blocked() {
        let tmp = TmpDir::new();
        for bad in [
            "../escaped.html",
            "sub/../../escaped.html",
            "..",
            "",
            "sub/..",
        ] {
            let err = resolve_artifact_path("s1", bad, &store(tmp.path()))
                .expect_err(&format!("name {bad:?} should not resolve"));
            assert_eq!(err.name(), "PathError", "{bad:?}: {err}");
            assert!(publish_artifact("s1", bad, "pwned", &store(tmp.path())).is_err());
        }
        // Nothing escaped: the root holds only what was never written.
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn ac_traversal_in_the_session_id_is_blocked() {
        let tmp = TmpDir::new();
        let outside = TmpDir::new();
        let outside_str = outside.path().to_string_lossy().into_owned();
        for bad in [
            "..",
            "../evil",
            "../../evil",
            outside_str.as_str(),
            "",
            "a/b",
        ] {
            let err = resolve_artifact_path(bad, "index.html", &store(tmp.path()))
                .expect_err(&format!("session id {bad:?} should not resolve"));
            assert_eq!(err.name(), "PathError", "{bad:?}: {err}");
            assert!(publish_artifact(bad, "index.html", "pwned", &store(tmp.path())).is_err());
            // An unaddressable id has published nothing, and says so rather
            // than failing.
            assert!(list_artifacts(bad, &store(tmp.path())).is_empty());
        }
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn one_session_cannot_read_or_write_anothers_artifacts() {
        let tmp = TmpDir::new();
        publish_artifact("victim", "secret.html", "<b>secret</b>", &store(tmp.path())).unwrap();
        // Reaching sideways out of "attacker" into "victim" is a path escape,
        // not a read.
        assert!(
            resolve_artifact_path("attacker", "../victim/secret.html", &store(tmp.path())).is_err()
        );
        assert!(list_artifacts("attacker", &store(tmp.path())).is_empty());
    }

    #[test]
    fn list_artifacts_is_newest_first_and_empty_for_a_session_that_published_none() {
        let tmp = TmpDir::new();
        assert!(list_artifacts("nobody", &store(tmp.path())).is_empty());
        publish_artifact("s2", "a.html", "a", &store(tmp.path())).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        publish_artifact("s2", "sub/b.css", "b", &store(tmp.path())).unwrap();
        let list = list_artifacts("s2", &store(tmp.path()));
        let mut sorted = list.iter().map(|a| a.name.clone()).collect::<Vec<_>>();
        sorted.sort();
        assert_eq!(sorted, vec!["a.html", "sub/b.css"]);
        assert_eq!(list[0].name, "sub/b.css");
    }

    #[test]
    fn a_percent_needing_name_encodes_per_segment_in_the_url() {
        let tmp = TmpDir::new();
        let art = publish_artifact(
            "s8",
            "my report.html",
            "<html>ok</html>",
            &store(tmp.path()),
        )
        .unwrap();
        assert_eq!(art.url, "/artifacts/s8/my%20report.html");
    }

    #[test]
    fn the_store_root_is_created_lazily_and_lives_outside_any_workspace() {
        // BOUGH_HOME points the DEFAULT root (no `root` option) at a temp
        // tree — the accessor path the server handlers use.
        let tmp = TmpDir::new();
        let home = tmp.path().to_string_lossy().into_owned();
        crate::paths::test_env::with_env(&[("BOUGH_HOME", Some(home.as_str()))], || {
            assert!(!artifacts_dir().exists());
            let opts = ArtifactStoreOptions::default();
            publish_artifact("s10", "index.html", "x", &opts).unwrap();
            assert!(tmp.path().join("artifacts/s10/index.html").is_file());
            assert_eq!(names("s10", &opts), vec!["index.html"]);
        });
    }
}
