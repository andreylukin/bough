//! `GET /skills`, `GET /skills/:name` — what is installed (port of
//! `src/server/skills.ts`).
//!
//! THE INVARIANT THIS HOLDS: **the filesystem is the source of truth, and this
//! endpoint reports it as it is — including the parts of it that are broken.**
//! There is no skills table, nothing is cached, and a listing is a fresh walk
//! of the source directories, so a skill dropped into `~/.bough/skills`
//! appears on the next request with no restart. A skill whose SKILL.md is
//! malformed is listed WITH its `error` rather than quietly omitted — the
//! panel showing the user their skills is the one place the mistake is
//! discoverable before a turn silently runs without it.
//!
//! There is no POST/PUT/DELETE, deliberately: a skill is a folder with a
//! markdown file in it, and an HTTP CRUD surface over it would be a second way
//! to write files with none of the properties of the first.

use serde_json::json;

use bough_core::errors::BoughError;
use bough_core::skills::{
    default_sources, list_skills, load_skill, sources_for, Skill, SkillSource,
};
use bough_core::types::AppCtx;

use crate::http::{handler, json as json_res, Handler};

/// The sources that apply to this request: the session's workspace when
/// `?session=` names one, else the workspace-independent set.
///
/// Degrading to [`default_sources`] rather than erroring is deliberate — a
/// listing must still answer for a session id that no longer exists, and the
/// `sources` array it returns is what tells the user which directories were
/// actually consulted.
fn sources_for_request(req: &axum::extract::Request, ctx: &AppCtx) -> Vec<SkillSource> {
    let session = req.uri().query().and_then(|q| {
        q.split('&')
            .find_map(|kv| kv.strip_prefix("session=").map(String::from))
    });
    let workspace = session.and_then(|id| {
        ctx.db
            .lock()
            .ok()?
            .get_session_runtime(&id)
            .ok()?
            .workspace
            .filter(|w| !w.is_empty())
    });
    match workspace {
        Some(w) => sources_for(std::path::Path::new(&w)),
        None => default_sources(),
    }
}

/// One row of `GET /skills` — the body is deliberately not in the listing.
/// `mcp` and `error` are omitted when empty/absent, matching `skills.ts::row`.
fn row(skill: &Skill) -> serde_json::Value {
    let mut out = json!({
        "name": skill.name,
        "description": skill.description,
        "source": skill.source,
        "dir": skill.dir,
    });
    let map = out.as_object_mut().expect("row is an object");
    if !skill.mcp.is_empty() {
        map.insert("mcp".into(), json!(skill.mcp));
    }
    if let Some(err) = &skill.error {
        map.insert("error".into(), json!(err));
    }
    out
}

/// `{source, dir}` as the wire carries it — `dir` is a string, not a path.
fn source_row(s: &SkillSource) -> serde_json::Value {
    json!({ "source": s.source, "dir": s.dir.to_string_lossy() })
}

/// `GET /skills` — every installed skill, name-sorted, first source winning.
///
/// `sources` rides along because "why is my skill not listed?" is almost always
/// answered by the directory it was expected in, and a client that only ever
/// sees an empty array cannot tell "nothing installed" from "looking in the
/// wrong place".
/// `?session=<id>` scopes the listing to that session's workspace, which is
/// what makes a project's checked-in skills and a project-scoped plugin
/// visible. Without it the workspace-independent sources are all that can be
/// answered — the panel always sends it, so the bare form is the CLI's.
pub fn list_skills_h() -> Handler {
    handler(|req, ctx, _params| async move {
        let sources = sources_for_request(&req, &ctx);
        let skills: Vec<_> = list_skills(&sources).iter().map(row).collect();
        let sources: Vec<_> = sources.iter().map(source_row).collect();
        Ok(json_res(
            &json!({ "skills": skills, "sources": sources }),
            200,
        ))
    })
}

/// `GET /skills/:name` — one skill, body included, `${SKILL_DIR}` resolved.
///
/// A 404 names the alternatives, because the usual cause is a typo or a folder
/// with no SKILL.md in it — both of which the list makes obvious once it is in
/// front of you.
pub fn get_skill() -> Handler {
    handler(|req, ctx, params| async move {
        let name = params.get("name").cloned().unwrap_or_default();
        // Same scoping as the listing: fetching a skill the listing showed
        // must not 404 because this handler looked in fewer directories.
        let sources = sources_for_request(&req, &ctx);
        match load_skill(&name, &sources) {
            Some(skill) => {
                let mut out = row(&skill);
                out.as_object_mut()
                    .expect("row is an object")
                    .insert("body".into(), json!(skill.body));
                Ok(json_res(&out, 200))
            }
            None => {
                let installed: Vec<String> = list_skills(&sources)
                    .iter()
                    .map(|s| format!("/{}", s.name))
                    .collect();
                let dirs: Vec<String> = sources
                    .iter()
                    .map(|s| s.dir.to_string_lossy().into_owned())
                    .collect();
                Err(BoughError::not_found(format!(
                    "no skill \"{name}\". A skill is a folder <dir>/{name}/SKILL.md in one of \
                     {}. {}",
                    dirs.join(" or "),
                    if installed.is_empty() {
                        "Nothing is installed.".to_string()
                    } else {
                        format!("Installed: {}.", installed.join(", "))
                    },
                )))
            }
        }
    })
}

#[cfg(test)]
// `bundled_sources` hands back a guard that serializes these tests against each
// other while they own the process-wide home; holding it across the awaits is
// the point, not an accident.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;

    /// The bundled skills are embedded in the binary and materialized on first
    /// use, so a real install always lists some — "empty" was the wave-1 stub's
    /// answer and would now mean discovery is broken.
    /// `BOUGH_HOME` is process-global, and `default_sources()` reads it per
    /// call — so these tests serialize on the crate-wide lock like every other
    /// env-touching test here (see artifacts.rs::HomeGuard).
    ///
    /// They also MATERIALIZE the bundled skills explicitly rather than trusting
    /// that it has already happened: `ensure_bundled_skills()` memoizes its
    /// destination in a `OnceLock`, so whichever test ran first decided that
    /// path process-wide — and if that test pointed `BOUGH_HOME` at a temp dir
    /// it then deleted, every later listing reads an absent directory and comes
    /// back empty. That is a test-ordering landmine, not a product bug (a real
    /// install's BOUGH_HOME does not move mid-process), and this is the cheapest
    /// way to be immune to it.
    fn bundled_sources() -> (std::sync::MutexGuard<'static, ()>, Vec<SkillSource>) {
        let lock = testutil::home_lock();
        let sources = default_sources();
        for s in &sources {
            if s.source == bough_core::skills::SkillSourceName::Bundled {
                let _ = bough_core::skills::materialize_bundled_skills(&s.dir);
            }
        }
        (lock, sources)
    }

    #[tokio::test]
    async fn the_listing_reports_the_bundled_skills_and_the_directories_searched() {
        let (_home, _sources) = bundled_sources();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/skills")).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;

        let skills = body["skills"].as_array().expect("skills is an array");
        assert!(!skills.is_empty(), "the bundled skills must be discovered");
        for s in skills {
            assert!(s["name"].is_string(), "every row names its skill");
            assert!(
                s["dir"].is_string(),
                "every row carries the folder ${{SKILL_DIR}} resolves to"
            );
            assert!(
                s.get("body").is_none(),
                "the body is deliberately not in the listing"
            );
        }
        // Name-sorted, first source winning.
        let names: Vec<&str> = skills.iter().map(|s| s["name"].as_str().unwrap()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "the listing is name-sorted");

        let sources = body["sources"].as_array().expect("sources is an array");
        assert!(
            !sources.is_empty(),
            "a client must be able to see where we looked"
        );
        assert!(sources
            .iter()
            .all(|s| s["dir"].is_string() && s["source"].is_string()));
    }

    /// `?session=` is what makes a project's checked-in skills visible at all.
    /// Without this the panel lists the workspace-independent sources and a
    /// user who checked a skill into their repo cannot see it anywhere —
    /// which is the exact question the `sources` array exists to answer.
    #[tokio::test]
    async fn a_session_scoped_listing_finds_the_workspaces_own_skills() {
        let (_home, _sources) = bundled_sources();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        let ws = std::env::temp_dir().join(format!("bough-srv-skills-{}", uuid::Uuid::new_v4()));
        let folder = ws.join(".agents").join("skills").join("shipit");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        std::fs::write(
            folder.join("SKILL.md"),
            "---\ndescription: ship the thing\n---\nSHIP BODY",
        )
        .unwrap();

        let session = "s-skills".to_string();
        {
            let db = fx.ctx.db.lock().unwrap();
            db.create_session(bough_core::schema::parts::Session {
                id: session.clone(),
                title: "t".into(),
                kind: bough_core::schema::parts::SessionKind::Root,
                created_at: 1_000,
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: Some(ws.to_string_lossy().into_owned()),
                origin_dir: None,
                base: None,
                model: None,
                effort: None,
                draft: None,
                context_tokens: None,
                cached_tokens: None,
                last_llm_at: None,
                outcome_ok: None,
            })
            .unwrap();
        }

        // Unscoped: the project skill is invisible, because nothing said which
        // project. This half is the control — without it the test would pass
        // even if the query parameter were ignored entirely.
        let bare = testutil::body_json(call.call(testutil::get("/skills")).await).await;
        let names = |body: &serde_json::Value| -> Vec<String> {
            body["skills"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert!(
            !names(&bare).contains(&"shipit".to_string()),
            "no session, no workspace, no project skills: {:?}",
            names(&bare)
        );

        let scoped = testutil::body_json(
            call.call(testutil::get(&format!("/skills?session={session}")))
                .await,
        )
        .await;
        assert!(
            names(&scoped).contains(&"shipit".to_string()),
            "the session's workspace must be searched: {:?}",
            names(&scoped)
        );
        let row = scoped["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "shipit")
            .unwrap()
            .clone();
        assert_eq!(
            row["source"], "project",
            "the row names the rung it came from"
        );
        assert!(
            scoped["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["dir"].as_str().unwrap().contains(".agents")),
            "the searched directories must include the workspace's own"
        );

        // And the body route agrees with the listing — a skill you can see and
        // cannot fetch is the failure this scoping could easily have caused.
        let res = call
            .call(testutil::get(&format!("/skills/shipit?session={session}")))
            .await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert!(body["body"].as_str().unwrap().contains("SHIP BODY"));
        // Unscoped, the same fetch is a 404 rather than a wrong answer.
        assert_eq!(
            call.call(testutil::get("/skills/shipit")).await.status(),
            404
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn one_skill_serves_its_body_with_skill_dir_resolved() {
        let (_home, _sources) = bundled_sources();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let listing = testutil::body_json(call.call(testutil::get("/skills")).await).await;
        let first = listing["skills"][0]["name"]
            .as_str()
            .expect("a bundled skill")
            .to_string();

        let res = call.call(testutil::get(&format!("/skills/{first}"))).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert_eq!(body["name"], first.as_str());
        let text = body["body"]
            .as_str()
            .expect("the body route serves the body");
        assert!(!text.is_empty(), "a loadable skill contributes a body");
        assert!(
            !text.contains("${SKILL_DIR}"),
            "${{SKILL_DIR}} is resolved before serving, not left for the client"
        );
    }

    #[tokio::test]
    async fn an_unknown_skill_is_a_404_naming_it_and_the_alternatives() {
        let (_home, _sources) = bundled_sources();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::get("/skills/definitely-not-a-skill"))
            .await;
        assert_eq!(res.status(), 404);
        let body = testutil::body_json(res).await;
        let msg = body["error"].as_str().unwrap();
        assert!(msg.contains("no skill \"definitely-not-a-skill\""), "{msg}");
        assert!(msg.contains("SKILL.md"), "{msg}");
        // The directories searched are the answer to "why is mine not listed".
        assert!(msg.contains('/'), "{msg}");
    }
}
