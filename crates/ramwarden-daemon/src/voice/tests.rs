use super::*;

fn cfg() -> config::Voice {
    config::Voice::default()
}

fn row(id: i64, idle: i64, p: f64, mb: u32) -> Row {
    Row {
        target: Target { browser: "b".into(), id, url: format!("https://example.org/{id}") },
        title: format!("Article {id}"),
        inactive_minutes: idle,
        eligible: true,
        closeable: true,
        status: "candidate",
        reason: String::new(),
        priority: 0,
        p_return: p,
        est_mb: mb,
        frecency: 0.0,
        related: false,
        class: "page",
        duplicate: false,
        p_rules: p,
        p_model: None,
    }
}

const DAY0: u64 = 20_000 * 86_400 + 3_600;

fn scene<'a>(rows: &'a [Row], closing: &'a [Target], now: Instant, unix: u64) -> Scene<'a> {
    Scene { rows, closing, percent: 60.0, now, unix }
}

fn at(start: Instant, mins: u64) -> Instant {
    start + Duration::from_secs(mins * 60)
}

#[test]
fn a_tab_about_to_close_is_asked_about_first_with_three_choices() {
    let v = Voice::default();
    let rows = vec![row(1, 200, 0.4, 600), row(2, 300, 0.05, 200)];
    let closing = vec![rows[1].target.clone()];
    let b = v.speak(&cfg(), &scene(&rows, &closing, Instant::now(), DAY0)).unwrap();
    assert_eq!(b.kind, Kind::CheckIn);
    assert_eq!(b.tab.as_ref().unwrap().target.id, 2, "the one the sweep would close, though smaller");
    assert!(b.text.contains("Article 2") && b.text.contains("5 hours"), "{}", b.text);
    assert_eq!(b.choices.iter().map(|c| c.id).collect::<Vec<_>>(), ["keep", "close", "later"]);
}

#[test]
fn without_a_tab_about_to_close_the_biggest_borderline_one_is_asked_about() {
    let v = Voice::default();
    let mut protected = row(4, 900, 0.3, 2000);
    protected.closeable = false;
    let rows = vec![row(1, 200, 0.4, 300), row(2, 200, 0.9, 900), row(3, 200, 0.3, 500), protected, row(5, 20, 0.3, 800)];
    let b = v.speak(&cfg(), &scene(&rows, &[], Instant::now(), DAY0)).unwrap();
    assert_eq!(b.tab.unwrap().target.id, 3, "not the sure-to-return one, the protected one or the recent one");
}

#[test]
fn the_same_bubble_is_returned_until_it_expires() {
    let v = Voice::default();
    let rows = vec![row(1, 200, 0.4, 300)];
    let t0 = Instant::now();
    let a = v.speak(&cfg(), &scene(&rows, &[], t0, DAY0)).unwrap();
    let b = v.speak(&cfg(), &scene(&rows, &[], at(t0, 5), DAY0)).unwrap();
    assert_eq!(a.id, b.id);
    assert!(b.expires_in_s <= 5 * 60);
    // Expired and unanswered: counts as "later", and the gap holds anything new.
    assert!(v.speak(&cfg(), &scene(&rows, &[], at(t0, 11), DAY0)).is_none());
}

#[test]
fn keep_holds_the_tab_out_of_closing_for_a_day_and_stops_the_questions() {
    let v = Voice::default();
    let rows = vec![row(1, 200, 0.1, 300)];
    let closing = vec![rows[0].target.clone()];
    let t0 = Instant::now();
    let b = v.speak(&cfg(), &scene(&rows, &closing, t0, DAY0)).unwrap();
    assert!(v.holds(&cfg(), &rows[0].target, t0), "held while the question is up");
    let Answered::Said(reply) = v.answer(&b.id, Pick::Keep, t0).unwrap() else { panic!() };
    assert!(!reply.is_empty());

    let mut sweep = closing.clone();
    assert_eq!(v.hold_back(&cfg(), &mut sweep, at(t0, 60)), 1);
    assert!(sweep.is_empty(), "kept tabs are not auto-closed");
    let mut sweep = closing.clone();
    assert_eq!(v.hold_back(&cfg(), &mut sweep, at(t0, 25 * 60)), 0, "the hold lasts keep_hours");

    let next = v.speak(&cfg(), &scene(&rows, &closing, at(t0, 120), DAY0));
    assert!(next.is_none_or(|b| b.kind != Kind::CheckIn), "not asked again while kept");
}

#[test]
fn close_hands_the_tab_back_to_the_caller_and_a_refusal_is_reported_honestly() {
    let v = Voice::default();
    let rows = vec![row(1, 200, 0.1, 300)];
    let closing = vec![rows[0].target.clone()];
    let t0 = Instant::now();
    let b = v.speak(&cfg(), &scene(&rows, &closing, t0, DAY0)).unwrap();
    let Answered::Close(tab) = v.answer(&b.id, Pick::Close, t0).unwrap() else { panic!() };
    assert_eq!(tab.target, rows[0].target);
    let reply = v.after_close(&tab.target, CloseResult::Refused, t0);
    assert!(reply.contains("couldn't"), "{reply}");
    assert!(!v.holds(&cfg(), &tab.target, t0), "a refusal is not a keep");
}

#[test]
fn an_answer_to_a_gone_bubble_is_stale() {
    let v = Voice::default();
    let rows = vec![row(1, 200, 0.4, 300)];
    let t0 = Instant::now();
    let b = v.speak(&cfg(), &scene(&rows, &[], t0, DAY0)).unwrap();
    assert_eq!(v.answer("nope", Pick::Keep, t0), Err(Stale));
    assert_eq!(v.answer(&b.id, Pick::Keep, at(t0, 11)), Err(Stale), "expired");
    assert!(v.answer(&b.id, Pick::Later, t0).is_ok());
    assert_eq!(v.answer(&b.id, Pick::Later, t0), Err(Stale), "answered once");
}

#[test]
fn later_waits_later_hours_before_asking_again() {
    let mut c = cfg();
    c.quips = false;
    let v = Voice::default();
    let rows = vec![row(1, 200, 0.4, 300)];
    let t0 = Instant::now();
    let b = v.speak(&c, &scene(&rows, &[], t0, DAY0)).unwrap();
    v.answer(&b.id, Pick::Later, t0).unwrap();
    assert!(v.speak(&c, &scene(&rows, &[], at(t0, 5 * 60), DAY0)).is_none_or(|b| b.kind == Kind::Reply));
    let again = v.speak(&c, &scene(&rows, &[], at(t0, 6 * 60 + 1), DAY0)).unwrap();
    assert_eq!(again.kind, Kind::CheckIn);
}

#[test]
fn the_gaps_space_bubbles_out_and_the_daily_cap_resets_at_midnight() {
    let mut c = cfg();
    c.max_per_day = 2;
    let v = Voice::default();
    let t0 = Instant::now();
    let first = v.speak(&c, &scene(&[], &[], t0, DAY0)).unwrap();
    assert_eq!(first.kind, Kind::Quip);
    v.answer(&first.id, Pick::Dismiss, t0).unwrap();
    assert!(v.speak(&c, &scene(&[], &[], at(t0, 20), DAY0)).is_none(), "min gap");
    assert!(v.speak(&c, &scene(&[], &[], at(t0, 60), DAY0)).is_none(), "quip gap is two hours");
    assert!(v.speak(&c, &scene(&[], &[], at(t0, 121), DAY0)).is_some());
    assert!(v.speak(&c, &scene(&[], &[], at(t0, 400), DAY0)).is_none(), "daily cap of 2");
    assert!(v.speak(&c, &scene(&[], &[], at(t0, 400), DAY0 + 86_400)).is_some(), "a new day");
}

#[test]
fn a_release_is_reported_first_with_its_estimated_size() {
    let v = Voice::default();
    let rows = vec![row(1, 200, 0.4, 300)];
    let t0 = Instant::now();
    v.note_release(4, 0, 1126, t0);
    v.note_release(0, 0, 0, t0);
    let b = v.speak(&cfg(), &scene(&rows, &[], t0, DAY0)).unwrap();
    assert_eq!(b.kind, Kind::Activity);
    assert!(b.text.contains('4') && b.text.contains("1.1 GB") && b.text.contains("roughly"), "{}", b.text);
}

#[test]
fn stale_news_is_dropped() {
    let mut c = cfg();
    c.quips = false;
    c.ask_about_tabs = false;
    let v = Voice::default();
    let t0 = Instant::now();
    v.note_release(0, 3, 500, t0);
    assert!(v.speak(&c, &scene(&[], &[], at(t0, 31), DAY0)).is_none());
}

#[test]
fn mute_silences_everything_saves_itself_and_expires() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("voice.json");
    let v = Voice::new(Some(path.clone()));
    let rows = vec![row(1, 200, 0.4, 300)];
    let t0 = Instant::now();
    assert!(!v.set_mute(true, Some(1.0), DAY0).is_empty());
    assert_eq!(v.mute_state(DAY0), (true, Some(DAY0 + 3600)));
    assert!(v.speak(&cfg(), &scene(&rows, &[], t0, DAY0)).is_none());

    let reloaded = Voice::new(Some(path));
    assert!(reloaded.mute_state(DAY0 + 10).0, "survives a restart");
    assert!(!reloaded.mute_state(DAY0 + 3600).0, "and expires");
    assert!(reloaded.speak(&cfg(), &scene(&rows, &[], t0, DAY0 + 3600)).is_some());

    v.set_mute(true, None, DAY0);
    assert_eq!(v.mute_state(DAY0 + 1_000_000), (true, None), "until unmuted");
    v.set_mute(false, None, DAY0);
    assert!(!v.mute_state(DAY0).0);
}

#[test]
fn turning_the_voice_off_or_the_questions_off_is_respected() {
    let rows = vec![row(1, 200, 0.4, 300)];
    let t0 = Instant::now();
    let mut off = cfg();
    off.enabled = false;
    assert!(Voice::default().speak(&off, &scene(&rows, &[], t0, DAY0)).is_none());
    let mut no_q = cfg();
    no_q.ask_about_tabs = false;
    assert_eq!(Voice::default().speak(&no_q, &scene(&rows, &[], t0, DAY0)).unwrap().kind, Kind::Quip);
    let mut no_jokes = cfg();
    no_jokes.quips = false;
    assert!(Voice::default().speak(&no_jokes, &scene(&[], &[], t0, DAY0)).is_none());
}

#[test]
fn quips_rotate_without_repeating_and_use_the_real_numbers() {
    let mut c = cfg();
    c.min_gap_minutes = 0.0;
    c.quip_every_minutes = 0.0;
    c.max_per_day = 100;
    c.max_quips_per_day = 100;
    let v = Voice::default();
    let rows: Vec<Row> = (0..40).map(|i| { let mut r = row(i, 5, 0.9, 100); r.closeable = false; r }).collect();
    let t0 = Instant::now();
    let mut said = Vec::new();
    for i in 0..6 {
        let now = t0 + Duration::from_secs(i * 61);
        let b = v.speak(&c, &scene(&rows, &[], now, DAY0)).unwrap();
        v.answer(&b.id, Pick::Dismiss, now).unwrap();
        said.push(b.text);
    }
    let unique: std::collections::HashSet<_> = said.iter().collect();
    assert_eq!(unique.len(), said.len(), "{said:?}");
    assert!(said.iter().any(|t| t.contains("40")), "a tab-count remark: {said:?}");
}

#[test]
fn idle_and_size_read_naturally() {
    assert_eq!(idle_text(1), "1 minute");
    assert_eq!(idle_text(45), "45 minutes");
    assert_eq!(idle_text(90), "2 hours");
    assert_eq!(idle_text(180), "3 hours");
    assert_eq!(idle_text(3 * 1440), "3 days");
    assert_eq!(mb_text(512), "512 MB");
    assert_eq!(mb_text(1126), "1.1 GB");
}

#[test]
fn no_more_than_three_jokes_a_day_by_default() {
    let mut c = cfg();
    c.min_gap_minutes = 0.0;
    c.quip_every_minutes = 0.0;
    let v = Voice::default();
    let t0 = Instant::now();
    let jokes = (0..10)
        .filter_map(|i| {
            let now = t0 + Duration::from_secs(i * 61);
            let b = v.speak(&c, &scene(&[], &[], now, DAY0))?;
            v.answer(&b.id, Pick::Dismiss, now).unwrap();
            Some(b)
        })
        .count();
    assert_eq!(jokes, 3);
}
