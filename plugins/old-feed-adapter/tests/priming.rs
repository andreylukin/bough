//! Invariant under test: `command_history` is COMPETENCE MEMORY. It is queried for priming and is
//! NEVER delivered — no agent should receive every shell command as an event (§14, §17) — while
//! `note_sections` is the other half: cited evidence naming the section it came from.

use crate::common;

use bough_plugin_old_feed_adapter::invariant::{check_steps, EVENT_PREFIX, FORBIDDEN_PREFIXES};
use bough_plugin_old_feed_adapter::{NoteQuery, PrimingQuery};
use common::{at, Fx, Which};

#[tokio::test]
async fn command_history_is_never_delivered_as_mail() {
    let fx = Fx::new(Which::Memory).await;
    common::standard_jungler(&fx.jungler_db);
    common::standard_bough(&fx.bough_db);
    let _sol = fx.sol_agent().await;

    let feed = fx.feed(fx.cfg());
    feed.sweep_at(at()).await.expect("a sweep");

    // The commands ARE there to be primed with…
    assert_eq!(
        feed.prime(&PrimingQuery::default())
            .await
            .expect("a priming query")
            .len(),
        3
    );

    // …and NONE of them is a step, of any kind.
    let steps = fx.all_steps().await;
    for step in &steps {
        for r in step.refs.iter() {
            for bad in FORBIDDEN_PREFIXES {
                assert!(
                    !r.as_str().starts_with(bad),
                    "step {} carries `{r}`; command memory is priming, never mail",
                    step.id
                );
            }
        }
    }
    let mail = fx.steps_of_kind("mail/delivered").await;
    assert_eq!(mail.len(), 3, "three jungler EVENTS, and nothing else");
    for step in &mail {
        assert!(
            step.cites
                .iter()
                .any(|c| c.r#ref.as_str().starts_with(EVENT_PREFIX)),
            "every delivered step is a jungler event"
        );
        let body = serde_json::to_string(&*step.body).expect("a body");
        assert!(!body.contains("cargo test"), "no command text rode along");
    }
    assert!(check_steps(&steps).is_ok());
}

#[tokio::test]
async fn prime_returns_command_history_filtered_by_repo_and_tag() {
    let fx = Fx::new(Which::Memory).await;
    common::standard_bough(&fx.bough_db);
    let feed = fx.feed(fx.cfg());

    let by_repo = feed
        .prime(&PrimingQuery {
            repo: Some("bough".to_string()),
            ..Default::default()
        })
        .await
        .expect("a priming query");
    assert_eq!(by_repo.len(), 2, "the jungler-repo row is filtered out");
    assert!(by_repo.iter().all(|c| c.repo == "bough"));
    // Newest first: the priming query answers "what did I just do here".
    assert_eq!(by_repo[0].cmd, "rg todo");

    let by_tag = feed
        .prime(&PrimingQuery {
            repo: Some("bough".to_string()),
            tags: vec!["cargo".to_string()],
            ..Default::default()
        })
        .await
        .expect("a priming query");
    assert_eq!(by_tag.len(), 1);
    assert_eq!(by_tag[0].cmd, "cargo test -p bough-plugin-ledger");
    assert_eq!(
        by_tag[0].tags,
        vec!["cargo".to_string(), "test".to_string()]
    );
    assert_eq!(by_tag[0].exit_code, Some(0));
    assert_eq!(by_tag[0].output_head, "ok");
}

#[tokio::test]
async fn notes_carry_a_cite_naming_the_note_section() {
    let fx = Fx::new(Which::Memory).await;
    common::standard_bough(&fx.bough_db);
    let feed = fx.feed(fx.cfg());

    let notes = feed
        .notes(&NoteQuery {
            contains: Some("seam".to_string()),
            limit: 0,
        })
        .await
        .expect("a notes query");
    assert_eq!(notes.len(), 1);
    let n = &notes[0];
    assert_eq!(n.note, 7);
    assert_eq!(n.ord, 0);
    assert_eq!(n.heading, "the seam");
    assert_eq!(n.author, "human");
    assert_eq!(
        n.cite.r#ref.as_str(),
        "note:7#0",
        "the cite names the SECTION, not the note"
    );

    let all = feed
        .notes(&NoteQuery::default())
        .await
        .expect("a notes query");
    assert_eq!(all.len(), 2);
    assert!(all[0].ord < all[1].ord, "a note reads in its own order");
}
