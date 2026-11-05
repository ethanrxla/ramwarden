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
        .or_else(|_|std::fs::read_to_string(config::config_dir().join("ramwarden/account-origin")))
        .unwrap_or_else(|_| "https://api.ramwarden.com".into());
    validated_base(value.trim())
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
async fn refresh_entitlement(http: &reqwest::Client, base: &str) -> Result<(), Error> {
    let token = config::device_token().ok_or("Connect this computer first")?;
    let key = ramwarden_core::wardens::PUBLIC_KEY.ok_or("This build has no trusted entitlement key")?;
    let response = http.get(format!("{base}/v1/entitlement")).bearer_auth(token).send().await?.error_for_status()?;
    if response.content_length().is_some_and(|n|n > 65536) { return Err("Entitlement is too large".into()); }
    let bytes = response.bytes().await?;
    if bytes.len() > 65536 { return Err("Entitlement is too large".into()); }
    let record: ramwarden_core::wardens::Entitlement = serde_json::from_slice(&bytes)?;
    if !record.verify(key) || record.issued_at > now()+300 {
        return Err("Entitlement signature or timestamp is invalid; existing pass retained".into());
    }
    let cfg = config::load()?;
    save_token(&cfg.db_path.with_file_name("entitlement.json"),std::str::from_utf8(&bytes)?)?;
    Ok(())
}

#[derive(Deserialize)]
struct Settings {
    enabled: bool,
    since: Option<u64>,
}
fn choice_path()->std::path::PathBuf {config::config_dir().join("ramwarden/usage-choice.json")}
async fn apply_choice(http:&reqwest::Client,base:&str)->Result<(),Error> {
    let path=choice_path();let Ok(bytes)=std::fs::read(&path) else{return Ok(())};
    let mut choice:Value=serde_json::from_slice(&bytes)?;
    if choice["pending"]!=true{return Ok(())}
    let token=config::device_token().ok_or("Connect this computer to save its sharing choice")?;
    http.post(format!("{base}/v1/usage/config")).bearer_auth(token).json(&json!({"enabled":choice["enabled"].as_bool().ok_or("Invalid sharing choice")?})).send().await?.error_for_status()?;
    choice["pending"]=json!(false);save_token(&path,&serde_json::to_string(&choice)?)?;Ok(())
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
            refresh_entitlement(http, base).await?;
            save_token(&config::config_dir().join("ramwarden/account-origin"),base)?;
            println!("This computer is already connected. Signed access refreshed.");
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
            refresh_entitlement(http, base).await?;
            save_token(&config::config_dir().join("ramwarden/account-origin"),base)?;
            println!("This computer is connected. Your confirmed sharing choice applies only to this computer.");
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
    if args.first().is_some_and(|s|s=="consent") {
        let enabled=match args.get(1).map(String::as_str){Some("on")=>true,Some("off")=>false,_=>return Err("Use consent on or consent off".into())};
        save_token(&choice_path(),&json!({"enabled":enabled,"pending":true}).to_string())?;
        return match apply_choice(&http,&base).await {Ok(())=>{println!("Usage sharing choice saved for this computer.");Ok(())},Err(_)=>{println!("Sharing choice saved locally; account confirmation is pending. No new sharing was enabled.");Ok(())}};
    }
    if args.first().is_some_and(|s| s == "login") {
        return login(&http, &base).await;
    }
    if args.first().is_some_and(|s| s == "refresh") {
        refresh_entitlement(&http,&base).await?;
        println!("Signed account access refreshed. No usage data uploaded.");
        return Ok(());
    }
    let watch = args.iter().any(|s| s == "--watch");
    loop {
        if let Err(error)=apply_choice(&http,&base).await {eprintln!("Usage sharing choice deferred: {error}");if watch {tokio::time::sleep(Duration::from_secs(300)).await;continue}else{return Err(error)}}
        if let Err(error) = refresh_entitlement(&http,&base).await { eprintln!("Account access refresh deferred: {error}"); }
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
