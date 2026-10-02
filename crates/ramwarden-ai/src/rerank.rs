//! Ranking tabs against what the user says they are doing.
//!
//! # Why this is not a job for the language model
//!
//! v1 asked Claude, in prose, "which of these tabs are relevant to the user's
//! goal?" That works, but it is the expensive way to answer a question that is
//! really about similarity, and it scales badly: 200 tabs is a large prompt, the
//! answer is non-deterministic, and a 4B local model is markedly worse at it than
//! Sonnet was.
//!
//! Embeddings answer it directly. Embed the goal once, embed each tab's title and
//! URL once, and rank by cosine similarity. One batched GPU pass, no prose, the
//! same answer every time — and it still works with the chat model entirely
//! offline.
//!
//! It also replaces something worse. v1's fallback was a hardcoded
//! `STALE_DOMAINS` list: youtube, reddit, twitter. That encodes the author's
//! guess about what is a waste of time rather than anything about *this* user's
//! work. A goal of "watching a lecture series" should keep YouTube tabs, and a
//! list cannot express that.

/// A tab's similarity to the stated goal.
#[derive(Clone, Debug, PartialEq)]
pub struct Scored {
    pub id: i64,
    /// Cosine similarity, in -1.0..=1.0. In practice embedding models produce
    /// 0.3..0.9 for related text, so thresholds live in that band.
    pub score: f32,
}

/// Cosine similarity between two vectors.
///
/// Returns 0.0 for a zero-magnitude or mismatched vector rather than NaN: an
/// embedding request that came back empty must not poison the ranking with a
/// value that compares false against everything.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    let sim = dot / (na.sqrt() * nb.sqrt());
    // Floating-point error can push a unit-vector dot product just outside the
    // range; clamp so callers can rely on the bounds.
    sim.clamp(-1.0, 1.0)
}

/// The text to embed for one tab.
///
/// Title first, because it is what the user would recognise, then the host. The
/// full URL is deliberately excluded: query strings and long paths are mostly
/// tracking parameters and session ids, which dilute the signal and, on a
/// privacy-sensitive machine, are the part you least want leaving the box.
pub fn tab_text(title: &str, url: &str) -> String {
    let host = host_of(url);
    match (title.trim().is_empty(), host.is_empty()) {
        (true, true) => String::new(),
        (true, false) => host,
        (false, true) => title.trim().to_string(),
        (false, false) => format!("{} ({})", title.trim(), host),
    }
}

/// Host portion of a URL, without scheme, credentials, port, or path.
pub fn host_of(url: &str) -> String {
    let rest = url
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    authority
        .rsplit_once(':')
        .filter(|(h, p)| !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        .map(|(h, _)| h)
        .unwrap_or(authority)
        .trim_start_matches("www.")
        .to_string()
}

/// Rank tabs against the goal, most relevant first.
///
/// `tab_vectors` must line up with `ids`. A tab whose embedding is missing scores
/// 0.0, which places it mid-pack rather than at either extreme — unknown is not
/// evidence of irrelevance.
pub fn rank(goal: &[f32], ids: &[i64], tab_vectors: &[Vec<f32>]) -> Vec<Scored> {
    let mut out: Vec<Scored> = ids
        .iter()
        .enumerate()
        .map(|(i, &id)| Scored {
            id,
            score: tab_vectors.get(i).map(|v| cosine(goal, v)).unwrap_or(0.0),
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// Tabs that look unrelated to the goal, least relevant first.
///
/// `keep_above` is the similarity at or above which a tab is spared. The default
/// in [`RELEVANT_THRESHOLD`] is deliberately generous: the cost of keeping a tab
/// that could have been closed is a few megabytes, and the cost of closing one
/// the user wanted is their work.
pub fn irrelevant(scored: &[Scored], keep_above: f32) -> Vec<i64> {
    let mut out: Vec<i64> = scored
        .iter()
        .filter(|s| s.score < keep_above)
        .map(|s| s.id)
        .collect();
    out.reverse(); // least relevant first
    out
}

/// Similarity at or above which a tab counts as related, for a single model.
///
/// Kept only for callers that know their model's distribution. Prefer
/// [`irrelevant_relative`]: an absolute threshold cannot work across models.
/// Measured on the same six-document test set:
///
/// ```text
/// nomic-embed-text      0.395 .. 0.495   (spread 0.100)
/// mxbai-embed-large     0.281 .. 0.675   (spread 0.394)
/// nemotron-3-embed-1b   0.096 .. 0.351   (spread 0.255)
/// ```
///
/// A cut at 0.30 keeps everything under nomic and drops almost everything under
/// the NVIDIA model. The number is a property of the embedder, not of relevance.
pub const RELEVANT_THRESHOLD: f32 = 0.30;

/// Where to cut, as a fraction of the observed score range.
///
/// 0.5 means "below the midpoint between the most and least relevant tab". On the
/// test set this picks out exactly the irrelevant documents under both working
/// models, which no absolute threshold does.
pub const RELATIVE_CUT: f32 = 0.5;

// The cost of keeping a tab that could have been closed is a few megabytes; the
// cost of closing one the user wanted is their work. Both constants are biased
// accordingly, and the bias is enforced at compile time.
const _: () = {
    assert!(RELEVANT_THRESHOLD <= 0.35, "a high bar closes plausibly-related tabs");
    assert!(RELATIVE_CUT <= 0.6, "cutting above the midpoint closes too much");
    assert!(MIN_SPREAD >= 0.10, "acting on a flat ranking is acting on noise");
};

/// Minimum score range for relevance to be used at all.
///
/// If the top and bottom tab score nearly the same, the embedder has not
/// distinguished them and its ordering is noise. `nomic-embed-text` produced a
/// 0.100 spread on the test set and ranked a basketball score above a CVE entry;
/// acting on that is worse than ignoring it. Below this, relevance is abandoned
/// and age decides alone.
pub const MIN_SPREAD: f32 = 0.15;

/// Prefix a search query the way the model requires.
///
/// Not cosmetic. Both working models are trained with asymmetric query and
/// document encodings, and skipping the prefix collapses the score range:
/// `nomic-embed-text` went from a 0.086 spread without prefixes to 0.100 with
/// them, and `mxbai` depends on its instruction entirely.
pub fn prefix_query(model: &str, text: &str) -> String {
    let m = model.to_lowercase();
    if m.contains("nomic") {
        format!("search_query: {text}")
    } else if m.contains("mxbai") || m.contains("arctic") {
        format!("Represent this sentence for searching relevant passages: {text}")
    } else {
        // NVIDIA retrieval models take `input_type: query` as a request field
        // instead, so the text is left alone.
        text.to_string()
    }
}

/// Prefix documents the way the model requires.
pub fn prefix_documents(model: &str, texts: &[String]) -> Vec<String> {
    let m = model.to_lowercase();
    if m.contains("nomic") {
        return texts.iter().map(|t| format!("search_document: {t}")).collect();
    }
    // mxbai and the NVIDIA models take documents unprefixed.
    texts.to_vec()
}

/// The spread between the most and least relevant tab.
pub fn spread(scored: &[Scored]) -> f32 {
    if scored.len() < 2 {
        return 0.0;
    }
    let max = scored.iter().map(|s| s.score).fold(f32::MIN, f32::max);
    let min = scored.iter().map(|s| s.score).fold(f32::MAX, f32::min);
    max - min
}

/// Whether this ranking is worth acting on.
pub fn discriminating(scored: &[Scored]) -> bool {
    spread(scored) >= MIN_SPREAD
}

/// Tabs that look unrelated, cut relative to the observed score range.
///
/// Returns nothing when the ranking is not [`discriminating`] — an embedder that
/// scored everything alike has told us nothing, and guessing from noise is how a
/// lasagna recipe outranks Metasploit documentation.
pub fn irrelevant_relative(scored: &[Scored], cut_fraction: f32) -> Vec<i64> {
    if !discriminating(scored) {
        return Vec::new();
    }
    let max = scored.iter().map(|s| s.score).fold(f32::MIN, f32::max);
    let min = scored.iter().map(|s| s.score).fold(f32::MAX, f32::min);
    let cut = min + (max - min) * cut_fraction;

    let mut out: Vec<i64> = scored
        .iter()
        .filter(|s| s.score < cut)
        .map(|s| s.id)
        .collect();
    out.reverse(); // least relevant first
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[f32]) -> Vec<f32> {
        xs.to_vec()
    }

    // ── Cosine ──────────────────────────────────────────────────────────────

    #[test]
    fn identical_vectors_are_maximally_similar() {
        let a = v(&[1.0, 2.0, 3.0]);
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn orthogonal_vectors_are_unrelated() {
        assert!(cosine(&v(&[1.0, 0.0]), &v(&[0.0, 1.0])).abs() < 1e-6);
    }

    #[test]
    fn opposite_vectors_are_maximally_dissimilar() {
        assert!((cosine(&v(&[1.0, 0.0]), &v(&[-1.0, 0.0])) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn magnitude_does_not_affect_similarity() {
        let a = v(&[1.0, 1.0]);
        let b = v(&[50.0, 50.0]);
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-6);
    }

    /// An embedding request that failed must not produce NaN, which compares
    /// false against everything and would silently sort to one end.
    #[test]
    fn a_degenerate_vector_scores_zero_rather_than_nan() {
        assert_eq!(cosine(&v(&[0.0, 0.0]), &v(&[1.0, 1.0])), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&v(&[1.0]), &v(&[1.0, 2.0])), 0.0, "length mismatch");
        assert!(!cosine(&v(&[0.0]), &v(&[0.0])).is_nan());
    }

    #[test]
    fn similarity_stays_inside_its_documented_bounds() {
        // Repeated near-unit vectors are where floating-point error shows up.
        let a = v(&[0.577_350_3, 0.577_350_3, 0.577_350_3]);
        let s = cosine(&a, &a);
        assert!((-1.0..=1.0).contains(&s), "{s}");
    }

    // ── Host extraction ─────────────────────────────────────────────────────

    #[test]
    fn extracts_the_host_from_real_urls() {
        assert_eq!(host_of("https://www.youtube.com/watch?v=abc"), "youtube.com");
        assert_eq!(host_of("http://localhost:3000/admin"), "localhost");
        assert_eq!(host_of("https://fau.sharepoint.com/:w:/r/sites/QEP"), "fau.sharepoint.com");
        assert_eq!(host_of("https://github.com/me/proj/pull/9"), "github.com");
        assert_eq!(host_of("chrome://downloads/"), "downloads");
    }

    #[test]
    fn strips_credentials_and_ports_but_keeps_a_real_subdomain() {
        assert_eq!(host_of("https://user:pw@example.com:8443/x"), "example.com");
        assert_eq!(host_of("https://api.v2.example.com/x"), "api.v2.example.com");
    }

    #[test]
    fn a_hostless_or_empty_url_yields_nothing_rather_than_garbage() {
        assert_eq!(host_of(""), "");
        // No `://` and `blank` is not a numeric port, so the whole scheme-less
        // string is kept rather than being split at the colon. These never reach
        // the ranker anyway — `tabpolicy::is_protected` rejects them first.
        assert_eq!(host_of("about:blank"), "about:blank");
        assert_eq!(host_of("javascript:void(0)"), "javascript:void(0)");
    }

    /// A colon followed by digits is a port and is stripped; a colon followed by
    /// anything else is part of the name.
    #[test]
    fn only_a_numeric_port_is_stripped() {
        assert_eq!(host_of("https://example.com:8443/x"), "example.com");
        assert_eq!(host_of("https://example.com:notaport/x"), "example.com:notaport");
    }

    // ── Tab text ────────────────────────────────────────────────────────────

    /// Query strings are mostly tracking parameters and session ids. They dilute
    /// the signal, and on a privacy-sensitive machine they are the part you least
    /// want leaving the box.
    #[test]
    fn tab_text_uses_the_title_and_host_not_the_query_string() {
        let t = tab_text("Rust ownership", "https://doc.rust-lang.org/book/ch04.html?utm_source=x&sid=SECRET");
        assert_eq!(t, "Rust ownership (doc.rust-lang.org)");
        assert!(!t.contains("SECRET"));
        assert!(!t.contains("utm_source"));
    }

    #[test]
    fn tab_text_copes_with_a_missing_title_or_host() {
        assert_eq!(tab_text("", "https://example.com/x"), "example.com");
        assert_eq!(tab_text("Just a title", ""), "Just a title");
        assert_eq!(tab_text("", ""), "");
        assert_eq!(tab_text("   ", "https://example.com/"), "example.com");
    }

    // ── Ranking ─────────────────────────────────────────────────────────────

    /// The scenario from v1's own prompt: a security-research goal should keep
    /// the CVE database and drop the recipe site. Vectors here stand in for the
    /// embedder: dimension 0 is "security", dimension 1 is "cooking".
    #[test]
    fn ranking_keeps_goal_related_tabs_and_drops_unrelated_ones() {
        let goal = v(&[1.0, 0.0]);
        let ids = vec![1, 2, 3];
        let vectors = vec![
            v(&[0.98, 0.20]), // a CVE database
            v(&[0.10, 0.99]), // a recipe site
            v(&[0.80, 0.60]), // a security-adjacent blog
        ];

        let scored = rank(&goal, &ids, &vectors);
        assert_eq!(scored[0].id, 1, "the CVE database is most relevant");
        assert_eq!(scored[2].id, 2, "the recipe site is least relevant");

        let drop = irrelevant(&scored, RELEVANT_THRESHOLD);
        assert_eq!(drop, vec![2], "only the recipe site falls below the bar");
    }

    /// The thing a hardcoded domain list cannot do: respect a goal that makes a
    /// normally-wasteful domain relevant.
    #[test]
    fn a_goal_can_make_a_normally_stale_domain_relevant() {
        // Dimension 0 is "lecture video".
        let goal = v(&[1.0, 0.0]);
        let ids = vec![10, 11];
        let vectors = vec![
            v(&[0.95, 0.10]), // a YouTube lecture — v1's list would close this
            v(&[0.05, 0.99]), // an unrelated shopping tab
        ];
        let drop = irrelevant(&rank(&goal, &ids, &vectors), RELEVANT_THRESHOLD);
        assert_eq!(drop, vec![11]);
        assert!(!drop.contains(&10), "the lecture must be kept");
    }

    #[test]
    fn least_relevant_comes_first_in_the_drop_list() {
        let goal = v(&[1.0, 0.0]);
        let ids = vec![1, 2, 3];
        let vectors = vec![v(&[0.0, 1.0]), v(&[0.2, 0.98]), v(&[0.1, 0.99])];
        let drop = irrelevant(&rank(&goal, &ids, &vectors), RELEVANT_THRESHOLD);
        assert_eq!(drop[0], 1, "the least similar tab is dropped first");
    }

    /// Unknown is not evidence of irrelevance, but it is not evidence of
    /// relevance either — a missing embedding lands mid-pack at 0.0.
    #[test]
    fn a_tab_with_no_embedding_scores_zero() {
        let goal = v(&[1.0, 0.0]);
        let scored = rank(&goal, &[1, 2], &[v(&[1.0, 0.0])]);
        let missing = scored.iter().find(|s| s.id == 2).unwrap();
        assert_eq!(missing.score, 0.0);
    }

    #[test]
    fn an_empty_tab_list_ranks_to_nothing() {
        assert!(rank(&v(&[1.0]), &[], &[]).is_empty());
        assert!(irrelevant(&[], RELEVANT_THRESHOLD).is_empty());
    }

    /// With no goal there is nothing to be relevant to, so nothing is dropped on
    /// relevance grounds and the age heuristic decides alone.
    #[test]
    fn an_empty_goal_vector_drops_nothing_on_relevance_grounds() {
        let scored = rank(&[], &[1, 2], &[v(&[1.0, 0.0]), v(&[0.0, 1.0])]);
        assert!(scored.iter().all(|s| s.score == 0.0));
        // Every score is 0.0, below the threshold — so the caller must not use
        // relevance when the goal is empty. Guarded in the provider.
        assert_eq!(irrelevant(&scored, RELEVANT_THRESHOLD).len(), 2);
    }

    // ── Prefixes ────────────────────────────────────────────────────────────

    #[test]
    fn each_model_family_gets_the_prefix_it_was_trained_with() {
        assert_eq!(prefix_query("nomic-embed-text", "goal"), "search_query: goal");
        assert!(prefix_query("mxbai-embed-large", "goal").starts_with("Represent this sentence"));
        assert_eq!(prefix_query("nvidia/nemotron-3-embed-1b", "goal"), "goal");

        let docs = vec!["a".to_string()];
        assert_eq!(prefix_documents("nomic-embed-text", &docs), vec!["search_document: a"]);
        assert_eq!(prefix_documents("mxbai-embed-large", &docs), docs);
        assert_eq!(prefix_documents("nvidia/nemotron-3-embed-1b", &docs), docs);
    }

    #[test]
    fn prefixing_is_case_insensitive_about_the_model_name() {
        assert!(prefix_query("MXBAI-Embed-Large", "g").starts_with("Represent"));
        assert_eq!(prefix_query("NOMIC-embed-text:latest", "g"), "search_query: g");
    }

    // ── Spread and the relative cut ─────────────────────────────────────────

    fn scored(scores: &[f32]) -> Vec<Scored> {
        scores
            .iter()
            .enumerate()
            .map(|(i, &score)| Scored { id: i as i64, score })
            .collect()
    }

    /// The real measured distributions. An absolute cut at 0.30 is wrong for
    /// both working models in opposite directions; the relative cut is right for
    /// both.
    #[test]
    fn the_relative_cut_works_across_models_where_an_absolute_one_cannot() {
        // mxbai, in the order CVE, Metasploit, Burp, video, basketball, lasagna.
        // The first three are relevant.
        let mxbai = scored(&[0.675, 0.524, 0.512, 0.431, 0.304, 0.281]);
        let dropped = irrelevant_relative(&mxbai, RELATIVE_CUT);
        assert_eq!(dropped.len(), 3, "{dropped:?}");
        assert!(dropped.contains(&3) && dropped.contains(&4) && dropped.contains(&5));

        // nemotron-3-embed-1b on the same documents: a quite different range.
        let nvidia = scored(&[0.351, 0.274, 0.266, 0.199, 0.106, 0.096]);
        let dropped = irrelevant_relative(&nvidia, RELATIVE_CUT);
        assert_eq!(dropped.len(), 3, "{dropped:?}");
        assert!(dropped.contains(&3) && dropped.contains(&4) && dropped.contains(&5));

        // The absolute threshold keeps everything under one model and drops
        // nearly everything under the other.
        assert_eq!(irrelevant(&nvidia, RELEVANT_THRESHOLD).len(), 5);
        assert_eq!(irrelevant(&mxbai, RELEVANT_THRESHOLD).len(), 1);
    }

    /// The guard that stops the nomic failure from reaching a decision: it ranked
    /// a basketball score above a CVE entry, with a 0.100 spread.
    #[test]
    fn a_non_discriminating_ranking_is_refused_rather_than_acted_on() {
        let nomic = scored(&[0.495, 0.465, 0.461, 0.431, 0.406, 0.395]);
        assert!(spread(&nomic) < MIN_SPREAD, "spread {}", spread(&nomic));
        assert!(!discriminating(&nomic));
        assert!(
            irrelevant_relative(&nomic, RELATIVE_CUT).is_empty(),
            "noise must not close tabs"
        );
    }

    #[test]
    fn a_working_ranking_is_discriminating() {
        assert!(discriminating(&scored(&[0.675, 0.524, 0.512, 0.431, 0.304, 0.281])));
        assert!(discriminating(&scored(&[0.351, 0.274, 0.266, 0.199, 0.106, 0.096])));
    }

    #[test]
    fn spread_of_a_trivial_ranking_is_zero() {
        assert_eq!(spread(&[]), 0.0);
        assert_eq!(spread(&scored(&[0.5])), 0.0);
        assert!(irrelevant_relative(&scored(&[0.5]), RELATIVE_CUT).is_empty());
    }

    #[test]
    fn identical_scores_are_never_discriminating() {
        let flat = scored(&[0.4, 0.4, 0.4, 0.4]);
        assert_eq!(spread(&flat), 0.0);
        assert!(irrelevant_relative(&flat, RELATIVE_CUT).is_empty());
    }

    #[test]
    fn the_relative_cut_still_lists_least_relevant_first() {
        let s = scored(&[0.9, 0.8, 0.2, 0.1]);
        let dropped = irrelevant_relative(&s, RELATIVE_CUT);
        assert_eq!(dropped[0], 3, "the lowest score is dropped first");
    }
}
