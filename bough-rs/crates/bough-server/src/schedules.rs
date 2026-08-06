//! Schedule routes (port of the REST surface of `src/schedules.ts`, wave-1
//! stub).
//!
//! v1-STUB per server.md §8 ("[] + 400 on create") — with one refinement the
//! honesty rule forces: the `schedules` TABLE is real and migrated, so the
//! listing serves the stored rows rather than pretending the table is empty.
//! What has NOT landed is the subsystem that mutates and fires them (spec
//! grammar CRUD + ticker, wave 2 row 2.8), so every mutation answers 400
//! "not yet ported" — except that a PATCH/DELETE of a schedule that does not
//! exist is still the 404 it would be in TS, so a typo'd id is diagnosed as
//! a typo and not as a missing feature.

use bough_core::errors::BoughError;

use crate::http::{handler, json, Handler};

fn not_yet() -> BoughError {
    BoughError::bad_request("schedule mutations are not yet ported in this build")
}

/// `GET /schedules` — bare `[Schedule]`, the stored rows.
pub fn list_schedules() -> Handler {
    handler(|_req, ctx, _params| async move {
        let rows = ctx.db.lock().unwrap().list_schedules()?;
        Ok(json(&rows, 200))
    })
}

/// `POST /schedules` — 400 "not yet ported".
pub fn create_schedule() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Err::<axum::response::Response, _>(not_yet())
    })
}

/// `PATCH /schedules/:id` — 404 for an unknown row, else 400 "not yet ported".
pub fn patch_schedule() -> Handler {
    handler(|_req, ctx, params| async move {
        require_schedule(&ctx, params.get("id").map(String::as_str).unwrap_or(""))?;
        Err::<axum::response::Response, _>(not_yet())
    })
}

/// `DELETE /schedules/:id` — same contract as PATCH.
pub fn delete_schedule() -> Handler {
    handler(|_req, ctx, params| async move {
        require_schedule(&ctx, params.get("id").map(String::as_str).unwrap_or(""))?;
        Err::<axum::response::Response, _>(not_yet())
    })
}

fn require_schedule(ctx: &bough_core::types::AppCtx, id: &str) -> Result<(), BoughError> {
    match ctx.db.lock().unwrap().get_schedule(id)? {
        Some(_) => Ok(()),
        None => Err(BoughError::not_found(format!("schedule {id} not found"))),
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use serde_json::json as j;

    #[tokio::test]
    async fn the_listing_is_a_bare_array_of_stored_rows() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/schedules")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!([]));
    }

    #[tokio::test]
    async fn create_is_a_400_not_yet_and_unknown_id_mutations_are_404() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req(
                "POST",
                "/schedules",
                Some(j!({"title": "t", "prompt": "p", "spec": "every:30m"})),
            ))
            .await;
        assert_eq!(res.status(), 400);
        let res = call.call(testutil::req("PATCH", "/schedules/nope", Some(j!({})))).await;
        assert_eq!(res.status(), 404);
        let res = call.call(testutil::req("DELETE", "/schedules/nope", None)).await;
        assert_eq!(res.status(), 404);
    }
}
