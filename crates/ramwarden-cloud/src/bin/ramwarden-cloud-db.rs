//! Check the Atlas connection and prepare the database.
//!
//!     ramwarden-cloud-db ping    # can the server reach the cluster?
//!     ramwarden-cloud-db init    # create the collections' indexes
//!     ramwarden-cloud-db stats   # document counts per collection
//!
//! Reads the connection string the same way the server does (see
//! `ramwarden_cloud::config`), so a passing `ping` here means the server will
//! connect too.

use ramwarden_cloud::{CloudConfig, Db, model};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cmd = std::env::args().nth(1).unwrap_or_else(|| "ping".into());
    if let Err(e) = run(&cmd).await {
        eprintln!("ramwarden-cloud-db {cmd}: {e}");
        std::process::exit(1);
    }
}

async fn run(cmd: &str) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = CloudConfig::load()?;
    let db = match Db::connect(&cfg).await {
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
            println!("ok: indexes ready in {:?}", cfg.database);
        }
        "stats" => {
            for name in [
                model::ACCOUNTS,
                model::DEVICES,
                model::DEVICE_CODES,
                model::REFRESH_TOKENS,
                model::PAYMENTS,
                model::USAGE_DAILY,
            ] {
                let n = db
                    .database()
                    .collection::<mongodb::bson::Document>(name)
                    .estimated_document_count()
                    .await?;
                println!("{name:16} {n}");
            }
        }
        other => return Err(format!("unknown command {other:?}; use ping, init or stats").into()),
    }
    Ok(())
}
