mod tailnet;
use serde_json::Value;
use std::{
    error::Error,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    process::Command,
    time::Duration,
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn banner() {
    if io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        println!(
            "\x1b[38;2;255;90;110m  ╭─ WARDEN ─────────────────────────────╮\n\x1b[38;2;180;130;255m  │  RAM guardian  ×  threat sentinel    │\n  ╰──────────────────────────────────────╯\x1b[0m"
        );
    } else {
        println!("WARDEN · Performance + security");
    }
}
fn config_dir() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("ramwarden")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            })
            .join("ramwarden")
    }
}
fn api_origin() -> Result<String> {
    let origin = std::env::var("RAMWARDEN_ACCOUNT_API")
        .or_else(|_| std::fs::read_to_string(config_dir().join("account-origin")))
        .unwrap_or_else(|_| "https://api.ramwarden.com".into());
    let origin = origin.trim();
    let u = reqwest::Url::parse(origin)?;
    let local = matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(u.scheme() == "https" || u.scheme() == "http" && local)
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || u.path() != "/"
    {
        return Err("Account address must be HTTPS or local development loopback".into());
    }
    Ok(origin.trim_end_matches('/').into())
}
async fn account(http: &reqwest::Client) -> Result<Value> {
    let token = std::fs::read_to_string(config_dir().join("device-token"))
        .map_err(|_| "Run ramwarden login to connect this computer first")?;
    let response = http
        .get(format!("{}/v1/device/account", api_origin()?))
        .bearer_auth(token.trim())
        .send()
        .await?;
    if !response.status().is_success() {
        return Err("Could not verify your plan. Run ramwarden login or try again when the service is available".into());
    }
    Ok(response.json().await?)
}
async fn local(http: &reqwest::Client, path: &str) -> Result<Value> {
    Ok(http
        .get(format!("http://127.0.0.1:7823/{path}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}
async fn status(http: &reqwest::Client) -> Result<()> {
    let m = local(http, "companion/state").await?;
    println!(
        "RAM    {:>5.1}%  · {:.1} / {:.1} GB · {}",
        m["percent"].as_f64().unwrap_or(0.),
        m["used_mb"].as_f64().unwrap_or(0.) / 1024.,
        m["total_mb"].as_f64().unwrap_or(0.) / 1024.,
        m["rung"].as_str().unwrap_or("unknown")
    );
    match local(http, "wardens/threat").await {
        Ok(t) => println!(
            "THREAT {}",
            t["status"]["headline"].as_str().unwrap_or("Unverified")
        ),
        Err(_) => println!("THREAT Report unavailable · protection unverified"),
    }
    Ok(())
}
async fn doctor(http: &reqwest::Client) {
    println!("Read-only checks · no files changed\n");
    match local(http, "health").await {
        Ok(v) => println!(
            "✓ Daemon responds · v{}",
            v["version"].as_str().unwrap_or("unknown")
        ),
        Err(_) => println!("! Daemon unavailable. Run ramwarden setup."),
    }
    match status(http).await {
        Ok(()) => {}
        Err(_) => println!("! Memory readings unavailable"),
    }
    match account(http).await {
        Ok(a) => println!(
            "✓ Account: {} · {} / {} computers",
            a["plan"].as_str().unwrap_or("unknown"),
            a["devices_used"],
            a["device_limit"]
        ),
        Err(e) => println!("! Account: {e}"),
    }
    println!(
        "{} Tailscale (optional)",
        if tailnet::inventory().await.is_ok() {
            "✓ Available on your own tailnet"
        } else {
            "— Not available; local installation still works"
        }
    );
}
fn helper() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.parent().map(|p| {
                p.join(if cfg!(windows) {
                    "ramwarden-usage-sync.exe"
                } else {
                    "ramwarden-usage-sync"
                })
            })
        })
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("ramwarden-usage-sync"))
}
fn prompt(text: &str) -> Result<String> {
    print!("{text} ");
    io::stdout().flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}
fn login() -> Result<()> {
    let origin = api_origin()?;
    if !Command::new(helper())
        .arg("login")
        .env("RAMWARDEN_ACCOUNT_API", origin)
        .status()?
        .success()
    {
        return Err("Sign-in did not finish. Run ramwarden login to retry".into());
    }
    Ok(())
}
async fn setup(http: &reqwest::Client) -> Result<()> {
    banner();
    println!("1 / 4 · Check this computer");
    doctor(http).await;
    if !io::stdin().is_terminal() {
        println!("Run ramwarden setup in an interactive terminal to continue.");
        return Ok(());
    }
    println!(
        "\n2 / 4 · Connect your account\nSign in with email, wallet or GitHub in your browser. No purchase or usage sharing is enabled by this step."
    );
    if account(http).await.is_err()
        && prompt("Connect this computer? [y/N]")?.eq_ignore_ascii_case("y")
    {
        login()?;
    }
    println!(
        "\n3 / 4 · Your companions\nUse the menu-bar app to show, resize and customize your wardens.\nDoctor reports installed engines separately from verified protection."
    );
    if cfg!(target_os = "macos")
        && prompt("Open the native setup guide? [y/N]")?.eq_ignore_ascii_case("y")
    {
        let app = PathBuf::from("/Applications/RamWarden Setup.app");
        if app.exists() {
            Command::new("open").arg(app).status()?;
        } else {
            println!(
                "Open RamWarden Setup from the downloaded disk image. The CLI is already installed; no second CLI download is needed."
            );
        }
    }
    println!(
        "\n4 / 4 · Other computers (optional)\nKeep your own tailnet. No Tailscale? Run this same setup on each computer.\nPlus supports 3 computers total; Pro supports 5. Existing devices count."
    );
    if prompt("Choose machines from your Tailscale network? [y/N]")?.eq_ignore_ascii_case("y") {
        tailnet::select(http).await?;
    }
    println!("\nReady: ramwarden status · ramwarden watch · ramwarden doctor");
    Ok(())
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()?;
    let alias = std::env::args()
        .next()
        .unwrap_or_default()
        .contains("ramwarden-doctor");
    let arg = if alias {
        "doctor".into()
    } else {
        std::env::args().nth(1).unwrap_or_else(|| {
            if io::stdin().is_terminal() {
                "setup".into()
            } else {
                "help".into()
            }
        })
    };
    match arg.as_str() {
        "setup" => setup(&http).await?,
        "login" => login()?,
        "doctor" => {
            banner();
            doctor(&http).await
        }
        "status" | "threat" => {
            banner();
            status(&http).await?;
        }
        "tailscale" => tailnet::select(&http).await?,
        "watch" => {
            let screen =
                io::stdout().is_terminal() && std::env::var_os("TERM").is_some_and(|v| v != "dumb");
            if screen {
                print!("\x1b[?1049h");
            }
            loop {
                if screen {
                    print!("\x1b[2J\x1b[H");
                }
                banner();
                println!("Live readings · Ctrl-C exits\n");
                status(&http)
                    .await
                    .unwrap_or_else(|_| println!("Daemon offline · retrying…"));
                tokio::select! {_=tokio::signal::ctrl_c()=>break,_=tokio::time::sleep(Duration::from_secs(5))=>{}}
            }
            if screen {
                print!("\x1b[?1049l");
                io::stdout().flush()?;
            }
        }
        "help" | "--help" | "-h" => {
            banner();
            println!(
                "ramwarden setup       Guided installation and account connection\nramwarden login       Connect this computer in your browser\nramwarden status      RAM and security at a glance\nramwarden watch       Live readings every 5 seconds; Ctrl-C exits\nramwarden doctor      Read-only installation checks\nramwarden tailscale   Choose machines and prepare an agent setup request\nramwarden threat      Security status\nramwarden-doctor      Shortcut for doctor"
            );
        }
        _ => return Err("Unknown command. Run ramwarden --help".into()),
    }
    Ok(())
}
