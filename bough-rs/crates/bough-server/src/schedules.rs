//! Schedule routes (port of the REST surface of `src/schedules.ts`).
//!
//! The validated CRUD lives in `bough_core::hostfn::schedule` — ONE validated
//! path shared with the `schedule.*` host fn, deliberately: a spec that parses
//! over HTTP but not from a program (or the reverse) is a bug nobody finds
//! until a schedule silently never fires. These handlers are the thin HTTP
//! adapters over it.
//!
//! `sessionId` — the conversation a firing reports back to — is NEVER accepted
//! from the wire. The create body has no such field (an injected one is
//! stripped by parsing) and the CRUD's deps leave it null on this path: a
//! schedule made over REST reports its firings to nobody.

use bough_core::hostfn::schedule::{
    schedule_create, schedule_patch, schedule_remove, ScheduleDeps,
};
use bough_core::schema::requests::{CreateScheduleBody, PatchScheduleBody};

use crate::http::{handler, json, parse_body, Handler};

/// `GET /schedules` — every schedule, in creation order, as a bare array.
///
/// No visibility derivation here: schedules are flat and few, and the panel
/// shows disabled ones too — that is how you re-enable one.
pub fn list_schedules() -> Handler {
    handler(|_req, ctx, _params| async move {
        let rows = ctx.db.lock().unwrap().list_schedules()?;
        Ok(json(&rows, 200))
    })
}

/// `POST /schedules` — 201 with the stored row, `nextRunAt` already computed
/// (from the ctx clock, per the shared CRUD's invariant).
pub fn create_schedule() -> Handler {
    handler(|req, ctx, _params| async move {
        let body: CreateScheduleBody = parse_body(req, None).await?;
        body.validate()?;
        let deps = ScheduleDeps { now: Some(ctx.now.clone()), ..Default::default() };
        let created = {
            let db = ctx.db.lock().unwrap();
            schedule_create(&*db, &body, &deps)?
        };
        Ok(json(&created, 201))
    })
}

/// `PATCH /schedules/:id` — partial update; `workspace: null` clears it; an
/// empty body is a legal no-op patch rather than a 400 about a missing object.
pub fn patch_schedule() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        let body: PatchScheduleBody = parse_body(req, Some(serde_json::json!({}))).await?;
        body.validate()?;
        let deps = ScheduleDeps { now: Some(ctx.now.clone()), ..Default::default() };
        let patched = {
            let db = ctx.db.lock().unwrap();
            schedule_patch(&*db, &id, &body, &deps)?
        };
        Ok(json(&patched, 200))
    })
}

/// `DELETE /schedules/:id` — 404 on an unknown id rather than a silent success.
pub fn delete_schedule() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        {
            let db = ctx.db.lock().unwrap();
            schedule_remove(&*db, &id)?;
        }
        Ok(json(&serde_json::json!({ "ok": true, "removed": id }), 200))
    })
}

// ---------------------------------------------------------------------------
// Tests — port of the REST section of src/schedules.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use bough_core::types::AppCtx;
    use serde_json::json as j;
    use std::sync::Arc;

    /// `Date.UTC(2026, 0, 15, 12, 0, 0)` — precomputed; the server crate has
    /// no chrono dependency and does not need one for a constant.
    fn t0() -> i64 {
        1_768_478_400_000
    }

    const HOUR: i64 = 3_600_000;

    /// The fixture ctx with the clock frozen at T0 — POST computes
    /// `nextRunAt` with the CTX clock, and the test pins it.
    fn frozen_ctx() -> AppCtx {
        let fx = testutil::fixture();
        let mut ctx = fx.ctx.clone();
        ctx.now = Arc::new(t0);
        ctx
    }

    #[tokio::test]
    async fn the_schedule_routes_are_crud_over_the_same_validated_path() {
        let ctx = frozen_ctx();
        let call = create_handler(ctx.clone(), CreateHandlerOptions::default());

        let created = call
            .call(testutil::req(
                "POST",
                "/schedules",
                Some(j!({"title": "nightly", "prompt": "run the suite", "spec": "every:2h"})),
            ))
            .await;
        assert_eq!(created.status(), 201);
        let schedule = testutil::body_json(created).await;
        assert_eq!(
            schedule["nextRunAt"],
            j!(t0() + 2 * HOUR),
            "the ctx clock is the one used"
        );
        let id = schedule["id"].as_str().unwrap().to_string();

        let listed = testutil::body_json(call.call(testutil::get("/schedules")).await).await;
        assert_eq!(
            listed.as_array().unwrap().iter().map(|s| s["id"].clone()).collect::<Vec<_>>(),
            vec![j!(id)]
        );

        let patched = call
            .call(testutil::req("PATCH", &format!("/schedules/{id}"), Some(j!({"enabled": false}))))
            .await;
        assert_eq!(patched.status(), 200);
        assert_eq!(testutil::body_json(patched).await["enabled"], j!(false));

        let removed = call.call(testutil::req("DELETE", &format!("/schedules/{id}"), None)).await;
        assert_eq!(removed.status(), 200);
        assert_eq!(testutil::body_json(removed).await, j!({"ok": true, "removed": id}));
        assert!(ctx.db.lock().unwrap().list_schedules().unwrap().is_empty());
    }

    #[tokio::test]
    async fn post_schedules_rejects_a_bad_spec_as_a_400_naming_the_grammar() {
        let ctx = frozen_ctx();
        let call = create_handler(ctx, CreateHandlerOptions::default());
        let res = call
            .call(testutil::req(
                "POST",
                "/schedules",
                Some(j!({"title": "t", "prompt": "p", "spec": "0 9 * * *"})),
            ))
            .await;
        assert_eq!(res.status(), 400);
        let body = testutil::body_json(res).await;
        assert!(
            body["error"].as_str().unwrap().contains("every:<N><m|h|d>"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn patch_and_delete_on_an_unknown_schedule_are_404s() {
        let ctx = frozen_ctx();
        let call = create_handler(ctx, CreateHandlerOptions::default());
        let patched = call.call(testutil::req("PATCH", "/schedules/nope", Some(j!({})))).await;
        assert_eq!(patched.status(), 404);
        let deleted = call.call(testutil::req("DELETE", "/schedules/nope", None)).await;
        assert_eq!(deleted.status(), 404);
    }

    #[tokio::test]
    async fn an_empty_patch_body_is_a_legal_no_op_not_a_400() {
        let ctx = frozen_ctx();
        let call = create_handler(ctx, CreateHandlerOptions::default());
        let created = testutil::body_json(
            call.call(testutil::req(
                "POST",
                "/schedules",
                Some(j!({"title": "t", "prompt": "p", "spec": "every:1h"})),
            ))
            .await,
        )
        .await;
        let id = created["id"].as_str().unwrap();

        // No body at all.
        let res = call.call(testutil::req("PATCH", &format!("/schedules/{id}"), None)).await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await["nextRunAt"], created["nextRunAt"]);
    }

    #[tokio::test]
    async fn a_session_id_on_the_wire_is_stripped_never_stored() {
        // The report-back target is stamped by the host fn from the calling
        // turn and only there — a REST client must not be able to point
        // another conversation's wake at itself.
        let ctx = frozen_ctx();
        let call = create_handler(ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req(
                "POST",
                "/schedules",
                Some(j!({
                    "title": "t", "prompt": "p", "spec": "every:1h",
                    "sessionId": "someone-elses-session"
                })),
            ))
            .await;
        assert_eq!(res.status(), 201);
        let body = testutil::body_json(res).await;
        assert_eq!(body["sessionId"], serde_json::Value::Null, "{body}");
        let stored = ctx.db.lock().unwrap().list_schedules().unwrap();
        assert_eq!(stored[0].session_id, None);
    }
}
