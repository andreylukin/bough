//! `GET /plugins`, `POST /plugins/:id` — what each plugin ships, and the
//! switch on every piece of it.
//!
//! Filesystem-backed like the hooks and skills routes: the listing walks
//! `~/.bough/plugins` and consults no table, so a plugin dropped in while the
//! server runs is listed on the next request.
//!
//! WHY THIS IS NOT THE HOOKS ROUTE WITH MORE ROWS. `GET /hooks` answers "what
//! Lua is loaded", and a plugin that has been switched off is not a hook source
//! at all — it has nothing there to list. This route answers the other
//! question, "what did I install and what is it allowed to do", which has to
//! include the parts that are currently doing nothing, or the switch would be a
//! one-way door.
//!
//! The toggle delegates to `plugins::set_enabled`, which routes a hook id back
//! into the hook store and rebuilds the interpreter. Nothing here knows which
//! file holds which switch, on purpose.

use bough_core::plugins::{list, set_enabled, Plugin};

use crate::http::{handler, json, parse_body, Handler};
use bough_core::errors::BoughError;

/// `GET /plugins` — every plugin, everything in it, and the directory that was
/// walked so "why is my plugin not listed?" has an answer on screen.
pub fn list_route() -> Handler {
    handler(|_req, _ctx, _params| async move {
        let plugins: Vec<Plugin> = list();
        Ok(json(
            &serde_json::json!({
                "plugins": plugins,
                "dir": bough_core::paths::plugins_dir().to_string_lossy(),
            }),
            200,
        ))
    })
}

#[derive(serde::Deserialize)]
struct ToggleBody {
    enabled: bool,
}

/// `POST /plugins/:id` `{enabled}` — turn one plugin, or one thing inside one,
/// on or off.
///
/// Answers with the WHOLE list for the same reason the hooks route does: a
/// plugin's switch changes every row under it, and a reload can change what the
/// hook rows say.
pub fn toggle() -> Handler {
    handler(|req, _ctx, params| async move {
        // Ids carry slashes (`acme/skills/review`), so they arrive
        // percent-encoded and the slash is part of the value.
        let id =
            crate::artifacts::decode_segments(params.get("id").map(String::as_str).unwrap_or(""));
        // The id indexes a listing, never a path. A `..` would write a
        // traversing string into the state file that no real id can ever match
        // — inert, but a lie on screen.
        if id.is_empty() || id.contains("..") {
            return Err(BoughError::bad_request(format!(
                "plugin id {id:?} is not one of the listed plugins or items."
            )));
        }
        if !known(&id) {
            return Err(BoughError::not_found(format!(
                "no plugin or item {id} in {}. `GET /plugins` lists what is installed.",
                bough_core::paths::plugins_dir().to_string_lossy(),
            )));
        }
        let body: ToggleBody = parse_body(req, None).await?;
        set_enabled(&id, body.enabled).map_err(|e| {
            BoughError::http(
                500,
                bough_core::errors::ErrorKind::Conflict,
                format!("could not write the plugin state: {e}"),
            )
        })?;
        Ok(json(&serde_json::json!({ "plugins": list() }), 200))
    })
}

/// Is this id a plugin, or something one of them ships?
fn known(id: &str) -> bool {
    list()
        .iter()
        .any(|p| p.name == id || p.items.iter().any(|i| i.id == id))
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
        /// A home with one plugin shipping one of each surface.
        fn new() -> HomeGuard {
            let lock = testutil::home_lock();
            let dir =
                std::env::temp_dir().join(format!("bough-plugins-r-{}", uuid::Uuid::new_v4()));
            let plugin = dir.join("plugins").join("acme");
            std::fs::create_dir_all(plugin.join("hooks")).unwrap();
            std::fs::create_dir_all(plugin.join("skills").join("review")).unwrap();
            std::fs::create_dir_all(plugin.join("extensions")).unwrap();
            std::fs::write(
                plugin.join("hooks").join("guard.lua"),
                "bough.api.create_autocmd(\"TurnEnd\", { callback = function() end })",
            )
            .unwrap();
            std::fs::write(
                plugin.join("skills").join("review").join("SKILL.md"),
                "---\ndescription: d\n---\nbody",
            )
            .unwrap();
            std::fs::write(plugin.join("extensions").join("gh.js"), "").unwrap();
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

    fn acme(body: &serde_json::Value) -> serde_json::Value {
        body["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "acme")
            .cloned()
            .expect("the plugin is listed")
    }

    fn item(plugin: &serde_json::Value, id: &str) -> bool {
        plugin["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .unwrap_or_else(|| panic!("{id} is listed: {plugin:?}"))["enabled"]
            .as_bool()
            .unwrap()
    }

    #[tokio::test]
    async fn the_listing_names_every_surface_and_says_what_is_on() {
        let _home = HomeGuard::new();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        let body = testutil::body_json(call.call(testutil::get("/plugins")).await).await;
        let acme = acme(&body);
        assert_eq!(acme["enabled"], serde_json::json!(true));
        let ids: Vec<&str> = acme["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "acme/guard.lua",
                "acme/skills/review",
                "acme/extensions/gh.js"
            ]
        );
        // Defaults are not changed by there being a switch: a plugin's hook is
        // off until asked for, its skill and its extension are not.
        assert!(!item(&acme, "acme/guard.lua"));
        assert!(item(&acme, "acme/skills/review"));
        assert!(item(&acme, "acme/extensions/gh.js"));
        assert!(body["dir"].as_str().unwrap().ends_with("plugins"));
    }

    #[tokio::test]
    async fn one_item_switches_without_touching_its_neighbours() {
        let _home = HomeGuard::new();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        let body = testutil::body_json(
            call.call(testutil::req(
                "POST",
                "/plugins/acme%2Fskills%2Freview",
                Some(serde_json::json!({ "enabled": false })),
            ))
            .await,
        )
        .await;
        let acme = acme(&body);
        assert!(!item(&acme, "acme/skills/review"));
        assert!(
            item(&acme, "acme/extensions/gh.js"),
            "its neighbour is untouched"
        );
        assert_eq!(
            acme["enabled"],
            serde_json::json!(true),
            "and so is the plugin itself"
        );

        // The switch is the SKILL's, not the listing's: it is gone from
        // `GET /skills` too, which is what `/review` resolves through.
        let skills = testutil::body_json(call.call(testutil::get("/skills")).await).await;
        assert!(
            !skills["skills"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["name"] == "review"),
            "{skills:?}"
        );
    }

    #[tokio::test]
    async fn a_plugin_switch_takes_everything_under_it_and_gives_it_back() {
        let _home = HomeGuard::new();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        // Turn the plugin's hook on first, so the round trip has something to
        // restore that is not just the default.
        call.call(testutil::req(
            "POST",
            "/plugins/acme%2Fguard.lua",
            Some(serde_json::json!({ "enabled": true })),
        ))
        .await;

        let off = testutil::body_json(
            call.call(testutil::req(
                "POST",
                "/plugins/acme",
                Some(serde_json::json!({ "enabled": false })),
            ))
            .await,
        )
        .await;
        assert_eq!(acme(&off)["enabled"], serde_json::json!(false));

        // Its hooks stop being a SOURCE, not merely stop being on: a disabled
        // plugin's Lua is not loaded, so the listeners it registered are gone.
        let hooks = testutil::body_json(call.call(testutil::get("/hooks")).await).await;
        assert!(
            !hooks["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|h| h["source"] == "acme"),
            "{hooks:?}"
        );

        let on = testutil::body_json(
            call.call(testutil::req(
                "POST",
                "/plugins/acme",
                Some(serde_json::json!({ "enabled": true })),
            ))
            .await,
        )
        .await;
        let acme = acme(&on);
        assert_eq!(acme["enabled"], serde_json::json!(true));
        assert!(
            item(&acme, "acme/guard.lua"),
            "the items kept their own switches while the plugin was off"
        );
    }

    #[tokio::test]
    async fn an_id_nobody_ships_is_refused_rather_than_written_into_the_state() {
        let _home = HomeGuard::new();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        let missing = call
            .call(testutil::req(
                "POST",
                "/plugins/nope",
                Some(serde_json::json!({ "enabled": false })),
            ))
            .await;
        assert_eq!(missing.status(), 404);

        let traversing = call
            .call(testutil::req(
                "POST",
                "/plugins/..%2F..%2Fevil",
                Some(serde_json::json!({ "enabled": false })),
            ))
            .await;
        assert!(
            traversing.status() == 400 || traversing.status() == 404,
            "a traversing id never reaches the state file: {}",
            traversing.status()
        );
    }
}
