//! Check the Atlas connection and prepare the database.
//!
//!     ramwarden-cloud-db ping    # can the server reach the cluster?
//!     ramwarden-cloud-db init    # create the collections' indexes
//!     ramwarden-cloud-db stats   # document counts per collection
//!
//!     ramwarden-cloud-db decisions sync [--watch MIN] [--no-titles] [--no-embed] [--no-recall]
//!     ramwarden-cloud-db decisions recent [HOST-OR-APP]
//!     ramwarden-cloud-db decisions why TEXT...     # "why did it close my Q3 plan tab?"
//!     ramwarden-cloud-db decisions similar ID      # decisions made for the same reasons
//!
//! `decisions` reads this machine's daemon config for the history database,
//! the Ollama embedder and the daemon's address. See
//! `docs/specs/decision-log.md`.
//!
//! Reads the connection string the same way the server does (see
//! `ramwarden_cloud::config`), so a passing `ping` here means the server will
//! connect too.

use ramwarden_cloud::decision_sync::{Embedder, Syncer, device_name, write_recall};
use ramwarden_cloud::decisions::{self, DecisionDoc, FEATURE_INDEX, TEXT_INDEX};
use ramwarden_cloud::{CloudConfig, MongoStore, model};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().cloned().unwrap_or_else(|| "ping".into());
    if let Err(e) = run(&cmd, &args[args.len().min(1)..]).await {
        eprintln!("ramwarden-cloud-db {cmd}: {e}");
        std::process::exit(1);
    }
}

async fn run(cmd: &str, rest: &[String]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cfg = CloudConfig::load()?;
    let db = match MongoStore::connect(&cfg).await {
        Ok(db) => db,
        Err(e) => {
            eprintln!(
                "could not reach the cluster. The usual causes are this machine's IP \
                 missing from Atlas Network Access, or a wrong database user password."
            );
            return Err(e.into());
        }
    };
    match cmd {
        "ping" => println!("ok: connected to database {:?}", cfg.database),
        "init" => {
            db.ensure_indexes().await?;
            db.ensure_decision_indexes(decisions::DEFAULT_TEXT_DIMS).await?;
            println!("ok: indexes ready in {:?}", cfg.database);
        }
        "support-indexes" => {
            db.ensure_support_indexes().await?;
            use futures::TryStreamExt;
            let indexes: Vec<_> = db.database().collection::<mongodb::bson::Document>(
                ramwarden_cloud::support::SUPPORT_CONVERSATIONS).list_indexes().await?.try_collect().await?;
            for index in indexes { println!("{:?}: {:?}",index.options,index.keys); }
            println!("ok: support indexes verified in {:?}",cfg.database);
        }
        "stats" => {
            for name in [
                model::ACCOUNTS,
                model::DEVICES,
                model::DEVICE_CODES,
                model::SESSIONS,
                model::CHALLENGES,
                model::EMAIL_CODES,
                model::PAYMENTS,
                model::USAGE_DAILY,
                ramwarden_cloud::support::SUPPORT_CONVERSATIONS,
                decisions::DECISIONS,
            ] {
                let n = db
                    .database()
                    .collection::<mongodb::bson::Document>(name)
                    .estimated_document_count()
                    .await?;
                println!("{name:22} {n}");
            }
        }
        "decisions" => decisions_cmd(&db, rest).await?,
        other => return Err(format!("unknown command {other:?}; use ping, init, stats or decisions").into()),
    }
    Ok(())
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn flag(rest: &[String], name: &str) -> bool {
    rest.iter().any(|a| a == name)
}

fn value(rest: &[String], name: &str) -> Option<String> {
    rest.iter().position(|a| a == name).and_then(|i| rest.get(i + 1)).cloned()
}

/// Words that are not flags or flag values.
fn words(rest: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in rest {
        if skip {
            skip = false;
        } else if a == "--watch" || a == "--history" {
            skip = true;
        } else if !a.starts_with("--") {
            out.push(a.clone());
        }
    }
    out
}

async fn decisions_cmd(db: &MongoStore, rest: &[String]) -> Result<(), BoxError> {
    let local = ramwarden_core::config::load().unwrap_or_default();
    let history = value(rest, "--history").map(Into::into).unwrap_or(local.db_path.clone());
    let embedder = (!flag(rest, "--no-embed")).then(|| Embedder::new(&local.ollama.host, &local.ollama.embed_model));
    let daemon = format!("http://{}:{}", local.server.host, local.server.port);
    let sub = rest.first().map(String::as_str).unwrap_or("sync");
    match sub {
        "sync" => {
            db.ensure_decision_indexes(decisions::DEFAULT_TEXT_DIMS).await?;
            let watch = value(rest, "--watch").and_then(|m| m.parse::<f64>().ok());
            let recall = !flag(rest, "--no-recall");
            let syncer = Syncer { db, history_path: history.clone(), device: device_name(), titles: !flag(rest, "--no-titles"), embedder };
            loop {
                let r = syncer.sync().await?;
                println!(
                    "synced {} decision(s) and {} ladder action(s) from {} as {:?}; {} embedded",
                    r.decisions, r.actions, history.display(), syncer.device, r.embedded
                );
                if let Some(e) = &r.embed_error {
                    eprintln!("no title embeddings this run (is Ollama running with {}?): {e}", local.ollama.embed_model);
                }
                if recall && let Some(e) = &syncer.embedder {
                    match write_recall(db, e, &daemon, &history).await {
                        Ok(n) => println!("recall: {n} open tab(s) have similar past closes"),
                        Err(err) => eprintln!("recall skipped: {err}"),
                    }
                }
                let Some(minutes) = watch else { break };
                tokio::time::sleep(std::time::Duration::from_secs_f64(minutes.max(1.0) * 60.0)).await;
            }
        }
        "recent" => {
            let subject = words(rest).get(1).cloned();
            for d in db.recent_decisions(subject.as_deref(), 30).await? {
                print_doc(&d, None);
            }
        }
        "why" => {
            let text = words(rest)[1..].join(" ");
            if text.is_empty() {
                return Err("usage: decisions why TEXT".into());
            }
            let e = embedder.ok_or("`why` needs the embedder; drop --no-embed")?;
            let v = e.embed(&[text]).await?.remove(0);
            for n in db.similar_decisions(TEXT_INDEX, &v, 10, Default::default()).await? {
                print_doc(&n.doc, Some(n.score));
            }
        }
        "similar" => {
            let id = words(rest).get(1).cloned().ok_or("usage: decisions similar ID")?;
            let d = db.decision(&id).await?.ok_or_else(|| format!("no decision {id}"))?;
            if d.vector.is_empty() {
                return Err(format!("{id} has no decision vector (apps and ladder rows do not)").into());
            }
            print_doc(&d, None);
            println!("--- decided for the same reasons:");
            let filter = mongodb::bson::doc! { "kind": &d.kind };
            for n in db.similar_decisions(FEATURE_INDEX, &d.vector, 11, filter).await? {
                if n.doc.id != d.id {
                    print_doc(&n.doc, Some(n.score));
                }
            }
        }
        other => return Err(format!("unknown decisions command {other:?}; use sync, recent, why or similar").into()),
    }
    Ok(())
}

fn print_doc(d: &DecisionDoc, score: Option<f64>) {
    let at = d.at.try_to_rfc3339_string().unwrap_or_default();
    let score = score.map(|s| format!(" [{s:.2}]")).unwrap_or_default();
    let outcome = d.outcome.as_deref().map(|o| format!(" -> {o}")).unwrap_or_default();
    println!("{}{score} {at} {} {} {} ({}){outcome}", d.id, d.kind, d.action, d.subject, d.result);
    if let Some(t) = &d.title {
        println!("    {t}");
    }
    println!("    {}", d.reason);
    if let Some(j) = &d.jev {
        println!("    jev: {j}");
    }
}
