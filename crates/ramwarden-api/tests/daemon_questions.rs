//! The server must accept every request the daemon actually sends.
//!
//! The catalog copies the daemon's question wording, and an exact match is the
//! point of it. This builds the requests with the daemon's own code, so rewording
//! a question there without updating `catalog.rs` fails this test instead of
//! silently turning the hosted tier off for that question.

use ramwarden_ai::jev::build_request;
use ramwarden_api::catalog::Catalog;
use ramwarden_core::judge::{Candidate, Intent};
use ramwarden_daemon::{appintel, ladderjudge, tabintel};

fn assert_accepted(what: &str, request: &serde_json::Value) {
    let catalog = Catalog::built_in();
    let questions = request["questions"].as_object().expect("questions object");
    assert!(!questions.is_empty(), "{what}: no questions");
    for (name, def) in questions {
        assert!(catalog.allows(name, def), "{what}: the server refuses `{name}`: {def}");
    }
}

fn candidate(name: &str, rank: usize) -> Candidate {
    Candidate {
        name: name.into(),
        cold_rank: rank,
        idle_minutes: 42.0,
        pss: 350_000_000,
        role: "chat".into(),
        has_window: true,
        established: 3,
        listening: false,
        reasons: vec!["quiet for 42 min".into()],
    }
}

#[test]
fn app_and_tab_questions_are_in_the_catalog() {
    assert_accepted("appintel", &build_request("jev-latest", "App.", &appintel::questions()));
    assert_accepted("tabintel", &build_request("jev-latest", "Tab.", &tabintel::questions()));
}

#[test]
fn a_full_ladder_judge_batch_fits_the_default_limits() {
    let names = [
        "Slack",
        "Discord",
        "WhatsApp",
        "zoom.us",
        "Google Chrome Helper (Renderer)",
        "Music",
        "Notes",
        "Preview",
    ];
    let candidates: Vec<Candidate> = names.iter().enumerate().map(|(i, n)| candidate(n, i)).collect();
    for intent in [Intent::Suspend, Intent::Kill] {
        let (state, questions) = ladderjudge::request(intent, &candidates);
        let request = build_request("jev-latest", &state, &questions);
        assert_accepted("ladderjudge", &request);

        // The defaults in `Config` must leave room for the largest batch.
        assert!(questions.len() <= 24, "{} questions", questions.len());
        assert!(state.len() <= 16_384, "{} bytes of state", state.len());
    }
}
