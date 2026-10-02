//! Exercise the real model tiers. Local always; cloud only if a key is given.
//!
//! cargo run -p ramwarden-ai --example live -- [path-to-key-file]

use std::time::{Duration, Instant};

use ramwarden_ai::client::{Nim, Ollama};
use ramwarden_ai::prompt::{Context, SYSTEM_PROMPT};
use ramwarden_ai::rerank::{self, RELEVANT_THRESHOLD};
use ramwarden_ai::secret::Secret;

fn demo_context() -> String {
    let processes = vec![
        ("brave".to_string(), 5160.0, "IDLE"),
        ("ChatGPT".to_string(), 3190.0, "IDLE"),
        ("cosmic-comp".to_string(), 1531.0, "PROTECTED"),
        ("Discord".to_string(), 476.0, "IDLE"),
    ];
    let protected = vec![
        ("cosmic-comp".to_string(), 1531.0, "desktop compositor — suspending it freezes the session".to_string()),
        ("claude".to_string(), 491.0, "AI agent session in progress".to_string()),
    ];
    let reclaimable = vec![(
        "Discord".to_string(), 476.0,
        "no CPU since the last sample; serving on port 6463".to_string(),
    )];
    let tabs = vec![
        (1i64, 6087i64, "https://www.youtube.com/watch?v=abc".to_string(), "Some video".to_string()),
        (2, 9000, "http://localhost:3000/admin".to_string(), "Dev server".to_string()),
        (3, 4725, "https://fau.sharepoint.com/:w:/r/sites/QEP/doc.aspx".to_string(), "QEP draft".to_string()),
        (4, 120, "https://nvd.nist.gov/vuln/detail/CVE-2026-1234".to_string(), "CVE-2026-1234".to_string()),
        (5, 3000, "https://www.allrecipes.com/lasagna".to_string(), "Lasagna recipe".to_string()),
    ];
    let terminals = vec![
        (226631i32, "bash".to_string(), true, vec![]),
        (1684418, "bash".to_string(), false, vec!["claude".to_string()]),
    ];

    Context {
        psi_some: 18.5,
        used_mb: 22_000.0,
        total_mb: 31_000.0,
        inactivity_minutes: 45,
        goal: "security research — reviewing CVEs",
        processes: &processes,
        protected: &protected,
        reclaimable: &reclaimable,
        tabs: &tabs,
        terminals: &terminals,
    }
    .render()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let user = demo_context();

    // ── Local ───────────────────────────────────────────────────────────────
    let ollama = Ollama::new(
        "http://127.0.0.1:11434",
        "nemotron-3-nano:4b",
        "mxbai-embed-large",
        Duration::from_secs(120),
    )?;
    let (ready, missing) = ollama.ready().await;
    println!("local ollama ready: {ready}  missing: {missing:?}");

    if ready {
        let t = Instant::now();
        match ollama.recommend(SYSTEM_PROMPT, &user).await {
            Ok(r) => {
                println!("\nLOCAL nemotron-3-nano:4b in {:?}", t.elapsed());
                println!("  tier:                 {}", r.tier);
                println!("  tabs_to_close:        {:?}", r.tabs_to_close);
                println!("  processes_to_suspend: {:?}", r.processes_to_suspend);
                println!("  idle_terminals:       {:?}", r.idle_terminals_to_close);
                println!("  workspaces_to_sort:   {}", r.workspaces_to_sort);
                println!("  estimated_freed_mb:   {}", r.estimated_ram_freed_mb);
                println!("  summary:              {}", r.summary);
            }
            Err(e) => println!("\nLOCAL failed: {e}"),
        }

        // ── Goal re-ranking, on the GPU ─────────────────────────────────────
        let goal = "security research — reviewing CVEs";
        let tabs: Vec<(i64, &str, &str)> = vec![
            (1, "Some video", "https://www.youtube.com/watch?v=abc"),
            (3, "QEP draft", "https://fau.sharepoint.com/:w:/r/sites/QEP/doc.aspx"),
            (4, "CVE-2026-1234", "https://nvd.nist.gov/vuln/detail/CVE-2026-1234"),
            (5, "Lasagna recipe", "https://www.allrecipes.com/lasagna"),
            (6, "Metasploit module docs", "https://docs.metasploit.com/docs/modules.html"),
        ];
        let docs: Vec<String> = tabs.iter().map(|(_, t, u)| rerank::tab_text(t, u)).collect();
        let t = Instant::now();
        let goal_v = ollama.embed_query(goal).await;
        let doc_v = ollama.embed(&docs).await;
        match (goal_v, doc_v) {
            (Ok(gv), Ok(dv)) if dv.len() == docs.len() && !gv.is_empty() => {
                let took = t.elapsed();
                let ids: Vec<i64> = tabs.iter().map(|(id, _, _)| *id).collect();
                let scored = rerank::rank(&gv, &ids, &dv);
                println!("\nGOAL RE-RANK ({} docs in {took:?}, dim {})", docs.len(), gv.len());
                println!("  goal: {goal:?}");
                let dropped = rerank::irrelevant_relative(&scored, rerank::RELATIVE_CUT);
                for s in &scored {
                    let (_, title, _) = tabs.iter().find(|(i, _, _)| *i == s.id).unwrap();
                    let verdict = if dropped.contains(&s.id) { "drop" } else { "KEEP" };
                    println!("    {:.3}  {verdict}  {title}", s.score);
                }
                println!("  spread {:.3}, discriminating: {}",
                    rerank::spread(&scored), rerank::discriminating(&scored));
                println!("  would close: {dropped:?}");
                let _ = RELEVANT_THRESHOLD;
            }
            (g, d) => println!("\nembedding failed: {:?} / {:?}", g.err(), d.err()),
        }
    }

    // ── Cloud, only when a key is supplied ──────────────────────────────────
    let Some(path) = std::env::args().nth(1) else {
        println!("\n(no key file given — cloud tier not exercised)");
        return Ok(());
    };
    let key = Secret::new(std::fs::read_to_string(&path)?.lines().next().unwrap_or("").trim());
    let nim = Nim::new(
        "https://integrate.api.nvidia.com",
        key,
        "nvidia/nemotron-3-super-120b-a12b",
        "nvidia/nemotron-3-embed-1b",
        Duration::from_secs(90),
    )?;
    println!("\nCLOUD key {}", nim.key_fingerprint());

    let t = Instant::now();
    match nim.recommend(SYSTEM_PROMPT, &user).await {
        Ok(r) => {
            println!("CLOUD {} in {:?}", nim.model, t.elapsed());
            println!("  tabs_to_close:        {:?}", r.tabs_to_close);
            println!("  processes_to_suspend: {:?}", r.processes_to_suspend);
            println!("  summary:              {}", r.summary);
        }
        Err(e) => println!("CLOUD chat failed: {e}"),
    }

    let t = Instant::now();
    match nim.embed(&["security research — reviewing CVEs".to_string()], "query").await {
        Ok(v) if !v.is_empty() => {
            println!("CLOUD embed {} -> dim {} in {:?}", nim.embed_model, v[0].len(), t.elapsed())
        }
        Ok(_) => println!("CLOUD embed returned nothing"),
        Err(e) => println!("CLOUD embed failed: {e}"),
    }
    Ok(())
}
