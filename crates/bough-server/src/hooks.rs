//! `GET /hooks`, `POST /hooks/:name` — what is installed, and the off switch.
//!
//! Filesystem-backed like the skills routes: the listing walks
//! `~/.bough/hooks` and consults no table, so a hook dropped in while the
//! server runs is listed on the next request. What the LIVE interpreter knows
//! — which files failed to parse, how many listeners each registered — is read
//! off the running host, so the panel shows the state that is actually in
//! force rather than a re-derivation of it.
//!
//! A DISABLED HOOK IS NOT LOADED, not skipped at dispatch. Toggling rebuilds
//! the interpreter (`hooks::set_enabled`), which is the only way to unregister
//! a listener that was created at load time.

use bough_core::hooks::{list_hooks, set_enabled, HookFile};

use crate::http::{handler, json, parse_body, Handler};
use bough_core::errors::BoughError;

/// `GET /hooks` — every `.lua` in the hooks directory, on or off, with the
/// directory that was walked so "why is my hook not listed?" has an answer on
/// screen.
pub fn list() -> Handler {
    handler(|_req, _ctx, _params| async move {
        let hooks: Vec<HookFile> = list_hooks();
        Ok(json(
            &serde_json::json!({
                "hooks": hooks,
                "dir": bough_core::hooks::hooks_dir().to_string_lossy(),
            }),
            200,
        ))
    })
}

#[derive(serde::Deserialize)]
struct ToggleBody {
    enabled: bool,
}

/// `POST /hooks/:name` `{enabled}` — turn one on or off.
///
/// Answers with the WHOLE list rather than the one row, because a reload can
/// change any of them: a file that was failing to parse may now be gone from
/// the errors, and a newly enabled one arrives with its listener count.
pub fn toggle() -> Handler {
    handler(|req, _ctx, params| async move {
        // The id is `<source>/<file>`, so it arrives percent-encoded and its
        // slash is part of the value, not a path separator.
        let name =
            crate::artifacts::decode_segments(params.get("name").map(String::as_str).unwrap_or(""));
        // The name indexes a listing, never a path: a `..` here would write a
        // traversing string into the disabled list, and a later reload would
        // compare it against file names that can never match — inert, but a
        // lie on screen. Refuse it at the door.
        if name.is_empty() || name.contains("..") {
            return Err(BoughError::bad_request(format!(
                "hook name {name:?} is not one of the listed files."
            )));
        }
        if !list_hooks().iter().any(|h| h.id == name) {
            return Err(BoughError::not_found(format!(
                "no hook {name} in {}. `GET /hooks` lists what is installed.",
                bough_core::hooks::hooks_dir().to_string_lossy(),
            )));
        }
        let body: ToggleBody = parse_body(req, None).await?;
        set_enabled(&name, body.enabled).map_err(|e| {
            // A 500 with the reason: this is the harness failing to write
            // its own state file, and the only useful thing to say is
            // which write failed and why.
            BoughError::http(
                500,
                bough_core::errors::ErrorKind::Conflict,
                format!("could not write the hook state: {e}"),
            )
        })?;
        Ok(json(&serde_json::json!({ "hooks": list_hooks() }), 200))
    })
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
        dir: std::path::PathBuf,
        previous: Option<String>,
    }

    impl HomeGuard {
        fn new(files: &[(&str, &str)]) -> HomeGuard {
            let lock = testutil::home_lock();
            let dir = std::env::temp_dir().join(format!("bough-hooks-r-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(dir.join("hooks")).unwrap();
            for (name, src) in files {
                std::fs::write(dir.join("hooks").join(name), src).unwrap();
            }
            let previous = std::env::var("BOUGH_HOME").ok();
            std::env::set_var("BOUGH_HOME", &dir);
            HomeGuard {
                _lock: lock,
                dir,
                previous,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var("BOUGH_HOME", v),
                None => std::env::remove_var("BOUGH_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[tokio::test]
    async fn the_listing_names_every_file_on_or_off_and_carries_a_load_error() {
        let _home = HomeGuard::new(&[
            (
                "a-good.lua",
                "bough.api.create_autocmd(\"TurnEnd\", { callback = function() end })",
            ),
            ("b-broken.lua", "this is not lua ((("),
        ]);
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        let body = testutil::body_json(call.call(testutil::get("/hooks")).await).await;
        let hooks = body["hooks"].as_array().unwrap();
        let local: Vec<&serde_json::Value> =
            hooks.iter().filter(|h| h["source"] == "local").collect();
        assert_eq!(local.len(), 2, "{hooks:?}");
        assert_eq!(local[0]["id"], "local/a-good.lua");
        assert_eq!(local[0]["enabled"], serde_json::json!(true), "yours are on");
        assert_eq!(local[1]["name"], "b-broken.lua");
        // Listed WITH its error, never omitted.
        assert!(
            local[1]["error"].as_str().is_some_and(|e| !e.is_empty()),
            "a file that cannot be parsed is listed with why: {:?}",
            local[1]
        );
        // The bundled ones are listed too. Every one that has behaviour of its
        // own is OFF — an upgrade must not start running code nobody turned on
        // — with exactly one exception: the two harness adapters, which only
        // ever act on a `.claude`/`.codex` config the user already wrote and
        // are inert on a machine with none (`hooks::sources::DEFAULT_ON`).
        let bundled: Vec<&serde_json::Value> =
            hooks.iter().filter(|h| h["source"] == "bundled").collect();
        assert!(!bundled.is_empty(), "bough ships hooks: {hooks:?}");
        for hook in &bundled {
            let id = hook["id"].as_str().unwrap_or_default();
            let expected = bough_core::hooks::sources::DEFAULT_ON.contains(&id);
            assert_eq!(
                hook["enabled"],
                serde_json::json!(expected),
                "{id} should arrive {}: {hook:?}",
                if expected { "on" } else { "off" }
            );
        }
        assert!(
            bundled
                .iter()
                .any(|h| h["enabled"] == serde_json::json!(false)),
            "the default-on list must stay an exception, not the rule: {bundled:?}"
        );
        assert!(body["dir"].as_str().unwrap().ends_with("hooks"));
    }

    #[tokio::test]
    async fn toggling_off_persists_and_the_listing_says_so() {
        let _home = HomeGuard::new(&[(
            "noisy.lua",
            "bough.api.create_autocmd(\"TurnEnd\", { callback = function() end })",
        )]);
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        let res = call
            .call(testutil::req(
                "POST",
                "/hooks/local%2Fnoisy.lua",
                Some(serde_json::json!({ "enabled": false })),
            ))
            .await;
        assert_eq!(res.status(), 200);
        let mine = |body: &serde_json::Value| -> serde_json::Value {
            body["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|h| h["id"] == "local/noisy.lua")
                .cloned()
                .expect("the local hook is listed")
        };
        let body = testutil::body_json(res).await;
        assert_eq!(mine(&body)["enabled"], serde_json::json!(false));

        // Persisted, not just remembered: the next listing reads it back.
        let again = testutil::body_json(call.call(testutil::get("/hooks")).await).await;
        assert_eq!(mine(&again)["enabled"], serde_json::json!(false));
        assert_eq!(
            mine(&again)["autocmds"],
            serde_json::json!(0),
            "a disabled hook is not loaded, so it has no listeners"
        );

        // And back on again.
        call.call(testutil::req(
            "POST",
            "/hooks/local%2Fnoisy.lua",
            Some(serde_json::json!({ "enabled": true })),
        ))
        .await;
        let on = testutil::body_json(call.call(testutil::get("/hooks")).await).await;
        assert_eq!(mine(&on)["enabled"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn a_name_that_is_not_listed_is_refused_rather_than_written_into_the_state() {
        let _home = HomeGuard::new(&[("only.lua", "")]);
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        let missing = call
            .call(testutil::req(
                "POST",
                "/hooks/local%2Fnope.lua",
                Some(serde_json::json!({ "enabled": false })),
            ))
            .await;
        assert_eq!(missing.status(), 404);

        let traversing = call
            .call(testutil::req(
                "POST",
                "/hooks/..%2F..%2Fevil.lua",
                Some(serde_json::json!({ "enabled": false })),
            ))
            .await;
        assert!(
            traversing.status() == 400 || traversing.status() == 404,
            "a traversing name never reaches the state file: {}",
            traversing.status()
        );
    }
}
