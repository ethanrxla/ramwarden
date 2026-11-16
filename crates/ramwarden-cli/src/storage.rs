//! `ramwarden storage`: disk space at the terminal.
//!
//! `ramwarden storage` says what is using the disk. `ramwarden storage free`
//! lists what RamWarden can clear and deletes only after a typed "y" at an
//! interactive terminal: it is one of the two places a clean-up can be
//! approved (the other is the Free button in the warden's bubble).

use std::io::{self, IsTerminal};
use std::time::Duration;

use serde_json::{Value, json};

use crate::Result;

const DAEMON: &str = "http://127.0.0.1:7823";

async fn get(http: &reqwest::Client, path: &str, timeout: u64) -> Result<Value> {
    Ok(http.get(format!("{DAEMON}/{path}")).timeout(Duration::from_secs(timeout)).send().await?.error_for_status()?.json().await?)
}

fn volumes(v: &Value) {
    for d in v["volumes"].as_array().into_iter().flatten() {
        let flag = match d["level"].as_str() {
            Some("critical") => "  !! nearly full",
            Some("low") => "  ! getting full",
            _ => "",
        };
        println!("{:<14} {:>9} free of {:>9}{flag}", d["name"].as_str().unwrap_or("?"), d["free"].as_str().unwrap_or("?"), d["total"].as_str().unwrap_or("?"));
    }
}

/// What is using the disk. `seconds` is the scan's time budget.
pub async fn show(http: &reqwest::Client, seconds: u64) -> Result<()> {
    println!("Measuring (up to {seconds}s)...");
    let v = get(http, &format!("storage/usage?refresh=true&seconds={seconds}"), seconds + 20).await?;
    volumes(&v);
    let row = |name: &str, r: &Value| {
        println!("  {:>9}{} {name}", r["size"].as_str().unwrap_or("?"), if r["complete"] == false { "+" } else { " " });
    };
    println!("\nLargest in your home folder");
    for f in v["home_folders"].as_array().into_iter().flatten() {
        row(f["name"].as_str().unwrap_or("?"), f);
    }
    for (kind, title) in [
        ("clear", "RamWarden can clear these after you approve (ramwarden storage free)"),
        ("review", "Your own files, never deleted by RamWarden"),
        ("guide", "Needs you: follow the steps"),
    ] {
        let rows: Vec<&Value> = v["places"].as_array().into_iter().flatten().filter(|r| r["kind"] == kind).collect();
        if rows.is_empty() {
            continue;
        }
        println!("\n{title}");
        for r in rows {
            row(r["label"].as_str().unwrap_or("?"), r);
            for step in r["steps"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                println!("             - {step}");
            }
        }
    }
    if v["partial"] == true {
        println!("\n+ means at least: the scan ran out of time. For a full count run: ramwarden storage 120");
    }
    Ok(())
}

/// Propose, show, ask, and only then delete.
pub async fn free(http: &reqwest::Client) -> Result<()> {
    let response = http
        .post(format!("{DAEMON}/storage/cleanup/propose"))
        .json(&json!({"items": [], "ask": false}))
        .timeout(Duration::from_secs(40))
        .send()
        .await?;
    let p: Value = response.json().await?;
    if p["ok"] != true {
        println!("{}", p["error"].as_str().unwrap_or("Nothing to clear."));
        return Ok(());
    }
    println!("RamWarden can clear {}:", p["size"].as_str().unwrap_or("?"));
    for l in p["lines"].as_array().into_iter().flatten() {
        println!("  {:>12.1} MB  {}", l["bytes"].as_f64().unwrap_or(0.0) / 1_048_576.0, l["label"].as_str().unwrap_or("?"));
    }
    println!("These are caches and temp files their apps rebuild. Your own files are not in the list.");
    let id = p["id"].as_str().unwrap_or_default();
    // Approval has to be a person at a keyboard, so a pipe or script declines.
    if !io::stdin().is_terminal() {
        let _ = http.post(format!("{DAEMON}/storage/cleanup/{id}/decline")).send().await;
        println!("Not cleared: run this in an interactive terminal to approve.");
        return Ok(());
    }
    if !crate::prompt("Clear them now? [y/N]")?.eq_ignore_ascii_case("y") {
        let _ = http.post(format!("{DAEMON}/storage/cleanup/{id}/decline")).send().await;
        println!("Nothing was changed.");
        return Ok(());
    }
    let done: Value =
        http.post(format!("{DAEMON}/storage/cleanup/{id}/apply")).timeout(Duration::from_secs(600)).send().await?.json().await?;
    println!("{}", done["reply"].as_str().or(done["error"].as_str()).unwrap_or("The daemon did not say what happened."));
    for note in done["notes"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        println!("  {note}");
    }
    volumes(&done);
    Ok(())
}
