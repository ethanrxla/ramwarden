//! Optional numeric diagnostics. No MongoDB credentials enter this client.
use ramwarden_core::{config, history::History};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::Write,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
type Error = Box<dyn std::error::Error + Send + Sync>;
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn api_base() -> Result<String, Error> {
    let value = std::env::var("RAMWARDEN_ACCOUNT_API")
        .unwrap_or_else(|_| "https://api.ramwarden.com".into());
    validated_base(&value)
}
fn validated_base(value: &str) -> Result<String, Error> {
    let url = reqwest::Url::parse(value)?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if (url.scheme() != "https" && !(url.scheme() == "http" && local))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("Account API must be an HTTPS origin or HTTP loopback origin".into());
    }
    Ok(value.trim_end_matches('/').into())
}
fn save_token(path: &Path, token: &str) -> Result<(), Error> {
    let parent = path.parent().ok_or("token directory")?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}
#[derive(Deserialize)]
struct Settings {
    enabled: bool,
    since: Option<u64>,
}
async fn upload(http: &reqwest::Client, base: &str) -> Result<usize, Error> {
    let token = config::device_token()
        .ok_or("Sign this computer in first with ramwarden-usage-sync login")?;
    let settings: Settings = http
        .get(format!("{base}/v1/usage/config"))
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if !settings.enabled {
        return Ok(0);
    }
    let since = settings.since.ok_or("Usage consent has no start time")?;
    let cfg = config::load()?;
    let history = History::open(&cfg.db_path)?;
    let today = now() / 86400 * 86400;
    let mut days = Vec::new();
    for back in 0..7 {
        let day = today.saturating_sub(back * 86400);
        if day.saturating_add(86400) > since {
            days.push(history.usage_day(day, since)?);
        }
    }
    let count = days.len();
    http.post(format!("{base}/v1/usage/daily"))
        .bearer_auth(&token)
        .json(&json!({"days":days}))
        .send()
        .await?
        .error_for_status()?;
    Ok(count)
}
async fn login(http: &reqwest::Client, base: &str) -> Result<(), Error> {
    if let Some(token) = config::device_token() {
        let response = http
            .get(format!("{base}/v1/usage/config"))
            .bearer_auth(token)
            .send()
            .await?;
        if response.status().is_success() {
            println!("This computer is already connected.");
            return Ok(());
        }
        if response.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Err("Could not verify the existing device sign-in. Try again later.".into());
        }
    }
    let platform = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    let name = ramwarden_cloud::decision_sync::device_name();
    let start: Value = http
        .post(format!("{base}/v1/device/start"))
        .json(&json!({"device_name":name,"platform":platform}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let raw = start["device_code"].as_str().ok_or("Missing device code")?;
    let url = start["verification_uri_complete"]
        .as_str()
        .ok_or("Missing sign-in URL")?;
    println!("Open this page to connect this computer:\n{url}\nWaiting for your approval…");
    let until = now() + start["expires_in"].as_u64().unwrap_or(600).min(900);
    while now() < until {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let response = http
            .post(format!("{base}/v1/device/token"))
            .json(&json!({"device_code":raw}))
            .send()
            .await?;
        let status = response.status();
        let body: Value = response.json().await?;
        if status.is_success() {
            let token = body["device_token"]
                .as_str()
                .filter(|t| t.starts_with("rwdt_"))
                .ok_or("Missing device token")?;
            save_token(
                &config::config_dir().join("ramwarden").join("device-token"),
                token,
            )?;
            println!(
                "This computer is connected. Usage sharing is controlled on your account page."
            );
            return Ok(());
        }
        if body["error"] != "authorization_pending" {
            return Err("Device sign-in expired or the account is at its device limit. Check your account page.".into());
        }
    }
    Err("Device sign-in timed out. Run login again when ready.".into())
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("RamWarden-usage-sync/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let base = api_base()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "login") {
        return login(&http, &base).await;
    }
    let watch = args.iter().any(|s| s == "--watch");
    loop {
        match upload(&http, &base).await {
            Ok(0) => println!("Usage sharing is off. No local activity was uploaded."),
            Ok(count) => println!("Uploaded {count} numeric daily summaries."),
            Err(error) if watch => eprintln!("Usage sync deferred: {error}"),
            Err(error) => return Err(error),
        }
        if !watch {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(300)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_origins_require_tls_or_exact_loopback() {
        for value in [
            "https://api.ramwarden.com",
            "http://localhost:8000",
            "http://127.0.0.1:8000",
        ] {
            assert!(validated_base(value).is_ok());
        }
        for value in [
            "http://localhost.evil:8000",
            "http://example.com",
            "https://user:password@example.com",
            "https://example.com/path",
            "https://example.com/?key=test",
        ] {
            assert!(validated_base(value).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn saved_device_tokens_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "warden-token-test-{}",
            ramwarden_cloud::store::random_token("")
        ));
        let path = dir.join("device-token");
        save_token(&path, "rwdt_test_fixture").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
