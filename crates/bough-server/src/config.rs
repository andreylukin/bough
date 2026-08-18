//! `GET /config`, `POST /config/:id` — everything the harness injects, and the
//! switch on each of it.
//!
//! ONE ROUTE, because it is one question. This replaces `GET /hooks` and
//! `GET /plugins`, which answered two halves of it and left skills and
//! extensions outside a plugin with no switch at all. `bough_core::config`
//! carries the argument; this is the wire.
//!
//! Filesystem-backed like the routes it replaces: the listing walks the
//! directories and consults no table, so something dropped in while the server
//! runs is listed on the next request. What the LIVE hook interpreter knows —
//! which files failed to parse, how many listeners each registered — is read
//! off the running host, so the panel shows the state actually in force rather
//! than a re-derivation of it.
//!
//! THE TOGGLE ANSWERS WITH THE WHOLE LISTING, never the one row: flipping a
//! group changes every row under it, and flipping a hook rebuilds the
//! interpreter, which can change what any hook row says. The reply IS the
//! refresh, with no second fetch to race it.

use std::path::{Path, PathBuf};

use bough_core::config::{list, set_enabled, ConfigGroup};
use bough_core::errors::BoughError;
use bough_core::types::AppCtx;

use crate::http::{handler, json, parse_body, Handler};

/// The workspace this request is about: the session's when `?session=` names
/// one, else the current directory.
///
/// Only the PROJECT tier depends on it — a skill in `.agents/skills` belongs to
/// the checkout. Degrading rather than erroring is deliberate, the same as the
/// skills route: a listing must still answer for a session id that no longer
/// exists, and the `dirs` on each group say which directories were walked.
fn workspace_for(req: &axum::extract::Request, ctx: &AppCtx) -> PathBuf {
    let session = req.uri().query().and_then(|q| {
        q.split('&')
            .find_map(|kv| kv.strip_prefix("session=").map(String::from))
    });
    session
        .and_then(|id| {
            ctx.db
                .lock()
                .ok()?
                .get_session_runtime(&id)
                .ok()?
                .workspace
                .filter(|w| !w.is_empty())
        })
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// `GET /config` — every group, everything under it, on or off.
pub fn list_route() -> Handler {
    handler(|req, ctx, _params| async move {
        let workspace = workspace_for(&req, &ctx);
        let groups: Vec<ConfigGroup> = list(&workspace);
        Ok(json(&serde_json::json!({ "groups": groups }), 200))
    })
}

#[derive(serde::Deserialize)]
struct ToggleBody {
    enabled: bool,
}

/// `POST /config/:id` `{enabled}` — turn one group, or one thing inside one,
/// on or off.
pub fn toggle() -> Handler {
    handler(|req, ctx, params| async move {
        // Ids carry slashes (`acme/skills/review`), so they arrive
        // percent-encoded and the slash is part of the value, not a separator.
        let id =
            crate::artifacts::decode_segments(params.get("id").map(String::as_str).unwrap_or(""));
        // The id indexes a listing, never a path. A `..` would write a
        // traversing string into the state file that no real id can ever match
        // — inert, but a lie on screen. Refuse it at the door.
        if id.is_empty() || id.contains("..") {
            return Err(BoughError::bad_request(format!(
                "id {id:?} is not one of the listed groups or items."
            )));
        }
        let workspace = workspace_for(&req, &ctx);
        if !known(&workspace, &id) {
            return Err(BoughError::not_found(format!(
                "nothing named {id} is installed. `GET /config` lists what is."
            )));
        }
        let body: ToggleBody = parse_body(req, None).await?;
        set_enabled(&id, body.enabled).map_err(|e| {
            // A 500 with the reason: this is the harness failing to write its
            // own state file, and the only useful thing to say is which write
            // failed and why.
            BoughError::http(
                500,
                bough_core::errors::ErrorKind::Conflict,
                format!("could not write the switchboard: {e}"),
            )
        })?;
        Ok(json(
            &serde_json::json!({ "groups": list(&workspace) }),
            200,
        ))
    })
}

fn known(workspace: &Path, id: &str) -> bool {
    bough_core::config::known(workspace, id)
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use std::sync::MutexGuard;

    /// `BOUGH_HOME` is process-global; the crate-wide lock serializes every
    /// test that redirects it.
    struct HomeGuard {
        _lock: MutexGuard<'static, ()>,
        home: std::path::PathBuf,
        prev: Option<String>,
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("BOUGH_HOME", v),
                None => std::env::remove_var("BOUGH_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    /// A home with one plugin shipping all three surfaces, one hook of the
    /// user's own, and one skill of the user's own.
    fn home() -> HomeGuard {
        let lock = testutil::home_lock();
        let home = std::env::temp_dir().join(format!("bough-cfg-http-{}", uuid::Uuid::new_v4()));
        let plugin = home.join("plugins/acme");
        std::fs::create_dir_all(plugin.join("hooks")).unwrap();
        std::fs::create_dir_all(plugin.join("skills/review")).unwrap();
        std::fs::write(plugin.join("hooks/guard.lua"), "").unwrap();
        std::fs::write(plugin.join("skills/review/SKILL.md"), "---\n---\nbody").unwrap();
        std::fs::create_dir_all(home.join("hooks")).unwrap();
        std::fs::write(home.join("hooks/mine.lua"), "").unwrap();
        std::fs::create_dir_all(home.join("skills/mine")).unwrap();
        std::fs::write(home.join("skills/mine/SKILL.md"), "---\n---\nbody").unwrap();
        let prev = std::env::var("BOUGH_HOME").ok();
        std::env::set_var("BOUGH_HOME", &home);
        HomeGuard {
            _lock: lock,
            home,
            prev,
        }
    }

    fn groups(body: &serde_json::Value) -> Vec<(String, bool)> {
        body["groups"]
            .as_array()
            .expect("groups")
            .iter()
            .map(|g| {
                (
                    g["id"].as_str().unwrap_or_default().to_string(),
                    g["enabled"].as_bool().unwrap_or(false),
                )
            })
            .collect()
    }

    fn item(body: &serde_json::Value, id: &str) -> serde_json::Value {
        body["groups"]
            .as_array()
            .expect("groups")
            .iter()
            .flat_map(|g| g["items"].as_array().cloned().unwrap_or_default())
            .find(|i| i["id"] == id)
            .unwrap_or_else(|| panic!("no item {id} in {body}"))
    }

    #[tokio::test]
    async fn the_listing_is_every_surface_under_its_group() {
        let _home = home();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let body = testutil::body_json(call.call(testutil::get("/config")).await).await;
        let ids: Vec<String> = groups(&body).into_iter().map(|(id, _)| id).collect();
        assert!(ids.contains(&"acme".to_string()), "{ids:?}");
        assert!(ids.contains(&"local".to_string()), "{ids:?}");
        // The two tiers that had no switch before: a hook you wrote and a
        // skill you wrote, both switchable from the one listing.
        assert_eq!(item(&body, "local/mine.lua")["surface"], "hook");
        assert_eq!(item(&body, "local/skills/mine")["surface"], "skill");
        assert_eq!(item(&body, "acme/skills/review")["surface"], "skill");
        // Defaults are reported, not re-decided.
        assert_eq!(item(&body, "acme/guard.lua")["enabled"], false);
        assert_eq!(item(&body, "local/mine.lua")["enabled"], true);
    }

    #[tokio::test]
    async fn a_toggle_answers_with_the_whole_listing_and_a_group_takes_the_lot() {
        let _home = home();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let body = testutil::body_json(
            call.call(testutil::req(
                "POST",
                "/config/local%2Fskills%2Fmine",
                Some(serde_json::json!({ "enabled": false })),
            ))
            .await,
        )
        .await;
        assert_eq!(
            item(&body, "local/skills/mine")["enabled"],
            false,
            "the reply IS the refresh"
        );
        // A group's switch outranks every item under it, and the items keep
        // their own for when it comes back.
        let body = testutil::body_json(
            call.call(testutil::req(
                "POST",
                "/config/acme",
                Some(serde_json::json!({ "enabled": false })),
            ))
            .await,
        )
        .await;
        let acme = groups(&body)
            .into_iter()
            .find(|(id, _)| id == "acme")
            .expect("still listed");
        assert!(!acme.1, "a group you switched off is still on screen");
        assert_eq!(item(&body, "acme/skills/review")["enabled"], true);
        assert_eq!(item(&body, "acme/skills/review")["live"], false);
    }

    #[tokio::test]
    async fn an_id_that_is_not_installed_is_refused_rather_than_recorded() {
        let _home = home();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        assert_eq!(
            call.call(testutil::req(
                "POST",
                "/config/nope",
                Some(serde_json::json!({ "enabled": false }))
            ))
            .await
            .status(),
            404
        );
        assert_eq!(
            call.call(testutil::req(
                "POST",
                "/config/..%2F..%2Fevil",
                Some(serde_json::json!({ "enabled": false }))
            ))
            .await
            .status(),
            400
        );
    }
}
