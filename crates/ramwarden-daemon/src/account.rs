//! Loopback-only device sign-in. The browser receives only the public approval
//! URL/code. Device codes and tokens stay in the daemon; paid access still
//! requires the server's signature and the matching account/device identity.

use axum::{
    Extension, Json, Router,
    routing::{get, post},
};
use ramwarden_core::{
    config::{self, Config},
    wardens::{self, Entitlement},
};
use ramwarden_platform::Secret;
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

struct Pending {
    secret: Secret,
    code: String,
    url: String,
    expires: Instant,
    next_poll: Instant,
    interval: Duration,
}

#[derive(Clone, Deserialize, Serialize)]
struct Identity {
    api: String,
    account_id: String,
    device_id: String,
}

#[derive(Default)]
struct Session {
    pending: Option<Pending>,
    message: Option<&'static str>,
    refresh_after: Option<Instant>,
}

pub struct Service {
    api: Option<Url>,
    http: Client,
    dir: PathBuf,
    entitlement: PathBuf,
    key: Option<String>,
    state: Mutex<Session>,
}

fn api_url(raw: &str) -> Option<Url> {
    let u = Url::parse(raw).ok()?;
    let local = u.host_str().is_some_and(crate::guard::is_loopback);
    (u.username().is_empty()
        && u.password().is_none()
        && u.query().is_none()
        && u.fragment().is_none()
        && u.path() == "/"
        && (u.scheme() == "https" || (u.scheme() == "http" && local)))
        .then_some(u)
}

impl Service {
    fn new(api: &str, dir: PathBuf, entitlement: PathBuf, key: Option<String>) -> Self {
        Self {
            api: api_url(api),
            dir,
            entitlement,
            key,
            http: Client::builder()
                .timeout(Duration::from_secs(12))
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .expect("HTTP client"),
            state: Mutex::new(Session::default()),
        }
    }

    fn url(&self, path: &str) -> Result<Url, &'static str> {
        self.api
            .as_ref()
            .and_then(|b| b.join(path).ok())
            .ok_or("Configure a secure account API first.")
    }

    fn identity(&self) -> Option<Identity> {
        let id: Identity =
            serde_json::from_slice(&std::fs::read(self.dir.join("account.json")).ok()?).ok()?;
        (id.api == self.api.as_ref()?.as_str()).then_some(id)
    }

    fn token(&self) -> Option<Secret> {
        let s = std::fs::read_to_string(self.dir.join("device-token")).ok()?;
        valid_token(s.trim()).then(|| Secret::new(s.trim().to_owned()))
    }

    fn verified(&self, id: &Identity) -> Option<Entitlement> {
        let ent = Entitlement::load_verified(&self.entitlement, self.key.as_deref());
        self.matches(&ent, id).then_some(ent)
    }

    fn matches(&self, ent: &Entitlement, id: &Identity) -> bool {
        self.key.as_deref().is_some_and(|k| ent.verify(k))
            && ent.account.as_deref() == Some(&id.account_id)
            && ent.device.as_deref() == Some(&id.device_id)
            && ent.issued_at <= wardens::now().saturating_add(60)
    }

    async fn view(&self) -> Value {
        let state = self.state.lock().await;
        if let Some(p) = &state.pending
            && p.expires > Instant::now()
        {
            return json!({"status":"pending", "user_code":p.code, "verification_uri_complete":p.url,
                "expires_in":p.expires.saturating_duration_since(Instant::now()).as_secs()});
        }
        if self.token().is_some()
            && let Some(id) = self.identity()
        {
            let ent = self.verified(&id);
            return json!({"status":"linked", "account_id":id.account_id, "device_id":id.device_id,
                "verified":ent.is_some(), "tier":ent.as_ref().and_then(|e| e.tier.as_deref()),
                "threat_unlocked":ent.as_ref().is_some_and(|e| e.allows(&wardens::THREAT, wardens::now())),
                "message":state.message});
        }
        if self.token().is_some() {
            return json!({"status":"unverified_device", "message":"An existing device credential needs to be linked again. Disconnect this app, then sign in."});
        }
        json!({"status":"signed_out", "message":state.message})
    }

    async fn start(&self) -> Result<(), &'static str> {
        let mut s = self.state.lock().await;
        if self.token().is_some() {
            return Err("Disconnect this app before signing in again.");
        }
        if s.pending
            .as_ref()
            .is_some_and(|p| p.expires > Instant::now())
        {
            return Ok(());
        }
        #[derive(Deserialize)]
        struct Start {
            device_code: String,
            user_code: String,
            verification_uri_complete: String,
            expires_in: u64,
            interval: u64,
        }
        let resp = self.http.post(self.url("v1/device/start")?).json(&json!({
            "device_name":ramwarden_wazuh::default_agent_name(), "platform":std::env::consts::OS,
        })).send().await.map_err(|_| "Cannot reach account service. Try again.")?;
        if !resp.status().is_success() {
            return Err("Account sign-in is unavailable. Try again later.");
        }
        let v: Start = body(resp).await?;
        let link = Url::parse(&v.verification_uri_complete).map_err(|_| "Invalid sign-in link.")?;
        if !approval_url(&link, self.api.as_ref().unwrap())
            || v.device_code.len() > 512
            || v.device_code.is_empty()
            || v.user_code.len() > 32
            || v.user_code.is_empty()
        {
            return Err("Invalid sign-in link.");
        }
        let interval = Duration::from_secs(v.interval.clamp(5, 60));
        s.pending = Some(Pending {
            secret: Secret::new(v.device_code),
            code: v.user_code,
            url: link.to_string(),
            expires: Instant::now() + Duration::from_secs(v.expires_in.min(600)),
            next_poll: Instant::now() + interval,
            interval,
        });
        s.message = None;
        Ok(())
    }

    async fn tick(&self) {
        let mut s = self.state.lock().await;
        if let Some(p) = &mut s.pending {
            if Instant::now() >= p.expires {
                s.pending = None;
                s.message = Some("Sign-in expired. Start again.");
                return;
            }
            if Instant::now() < p.next_poll {
                return;
            }
            p.next_poll = Instant::now() + p.interval;
            let Ok(url) = self.url("v1/device/token") else {
                return;
            };
            let Ok(resp) = self
                .http
                .post(url)
                .json(&json!({"device_code":p.secret.expose()}))
                .send()
                .await
            else {
                return;
            };
            let status = resp.status();
            let Ok(v) = body::<Value>(resp).await else {
                return;
            };
            if !status.is_success() {
                match v["error"].as_str() {
                    Some("authorization_pending") => return,
                    Some("slow_down") => {
                        p.interval =
                            (p.interval + Duration::from_secs(5)).min(Duration::from_secs(60));
                        return;
                    }
                    _ if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS => {
                        return;
                    }
                    _ => {
                        s.pending = None;
                        s.message = Some(
                            "Sign-in expired or device limit reached. Start again on your account page.",
                        );
                        return;
                    }
                }
            }
            let saved = self.save_claim(&v);
            s.pending = None;
            if saved.is_err() {
                s.message = Some(
                    "Could not save device sign-in. Remove the new device on your account page before retrying.",
                );
                return;
            }
            s.refresh_after = None;
        }
        if s.refresh_after.is_none_or(|t| t <= Instant::now()) {
            s.refresh_after = Some(Instant::now() + Duration::from_secs(300));
            if let (Some(id), Some(token)) = (self.identity(), self.token()) {
                s.message = self.refresh(&id, &token).await.err();
            }
        }
    }

    fn save_claim(&self, v: &Value) -> Result<(), ()> {
        let token = v["device_token"]
            .as_str()
            .filter(|t| valid_token(t))
            .ok_or(())?;
        let id = |k: &str| {
            v[k].as_str()
                .filter(|s| !s.is_empty() && s.len() <= 128)
                .map(str::to_owned)
                .ok_or(())
        };
        let id = Identity {
            api: self.api.as_ref().ok_or(())?.to_string(),
            account_id: id("account_id")?,
            device_id: id("device_id")?,
        };
        // An old signed record can never unlock a newly linked account.
        remove(&self.entitlement).map_err(|_| ())?;
        private_write(
            &self.dir.join("account.json"),
            &serde_json::to_vec(&id).map_err(|_| ())?,
        )
        .map_err(|_| ())?;
        private_write(&self.dir.join("device-token"), token.as_bytes()).map_err(|_| ())
    }

    async fn refresh(&self, id: &Identity, token: &Secret) -> Result<(), &'static str> {
        let resp = self
            .http
            .get(self.url("v1/entitlement")?)
            .bearer_auth(token.expose())
            .send()
            .await
            .map_err(|_| "Account service offline; showing the last verified plan.")?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            self.clear()
                .map_err(|_| "Could not clear revoked credentials.")?;
            return Err("This device was revoked. Sign in again.");
        }
        if !resp.status().is_success() {
            return Err("Plan verification unavailable. Try again later.");
        }
        let ent: Entitlement = body(resp).await?;
        if !self.matches(&ent, id) {
            return Err("Plan signature or device identity could not be verified.");
        }
        let bytes = serde_json::to_vec(&ent).map_err(|_| "Invalid plan response.")?;
        private_write(&self.entitlement, &bytes).map_err(|_| "Could not save verified plan.")
    }

    fn clear(&self) -> std::io::Result<()> {
        // Lock paid features before forgetting identity; fail closed on IO errors.
        remove(&self.entitlement)?;
        remove(&self.dir.join("device-token"))?;
        remove(&self.dir.join("account.json"))
    }
}

fn valid_token(t: &str) -> bool {
    t.strip_prefix("rwdt_")
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn approval_url(link: &Url, api: &Url) -> bool {
    let secure = link.scheme() == "https"
        || (api.scheme() == "http"
            && link.scheme() == "http"
            && link.host_str().is_some_and(crate::guard::is_loopback));
    secure
        && link.username().is_empty()
        && link.password().is_none()
        && link.path() == "/link.html"
        && link.fragment().is_none()
}

async fn body<T: serde::de::DeserializeOwned>(
    mut resp: reqwest::Response,
) -> Result<T, &'static str> {
    let mut bytes = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|_| "Account response unavailable.")?
    {
        if bytes.len() + chunk.len() > 65536 {
            return Err("Account response too large.");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid account response.")
}

fn private_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing parent"))?;
    std::fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

pub fn routes(cfg: &Config) -> Router<crate::hub::AppState> {
    let service = Arc::new(Service::new(
        &cfg.wardens.threat.cloud_url,
        config::config_dir().join("ramwarden"),
        cfg.db_path.with_file_name("entitlement.json"),
        wardens::PUBLIC_KEY.map(str::to_owned),
    ));
    let weak = Arc::downgrade(&service);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let Some(s) = weak.upgrade() else { break };
            s.tick().await;
        }
    });
    Router::new()
        .route("/account", get(status))
        .route("/account/start", post(start))
        .route("/account/refresh", post(refresh))
        .route("/account/disconnect", post(disconnect))
        .layer(Extension(service))
}

async fn status(Extension(s): Extension<Arc<Service>>) -> Json<Value> {
    Json(s.view().await)
}
async fn start(Extension(s): Extension<Arc<Service>>) -> (StatusCode, Json<Value>) {
    match s.start().await {
        Ok(()) => (StatusCode::OK, Json(s.view().await)),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({"error":e}))),
    }
}
async fn refresh(Extension(s): Extension<Arc<Service>>) -> Json<Value> {
    s.state.lock().await.refresh_after = None;
    s.tick().await;
    Json(s.view().await)
}
async fn disconnect(Extension(s): Extension<Arc<Service>>) -> (StatusCode, Json<Value>) {
    let mut state = s.state.lock().await;
    match s.clear() {
        Ok(()) => {
            *state = Session::default();
            (StatusCode::OK, Json(json!({"status":"signed_out"})))
        }
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"Could not remove this app's credentials."})),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    use ed25519_dalek::{Signer, SigningKey};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Fake {
        approved: AtomicBool,
        revoked: AtomicBool,
        polls: AtomicUsize,
        ent: Entitlement,
    }
    fn signed(account: &str, device: &str) -> (Entitlement, String) {
        let key = SigningKey::from_bytes(&[7; 32]);
        let mut e = Entitlement {
            v: 2,
            account: Some(account.into()),
            device: Some(device.into()),
            issued_at: wardens::now(),
            tier: Some("pro".into()),
            status: Some("active".into()),
            wardens: [
                ("ram".into(), wardens::now() + 600),
                ("threat".into(), wardens::now() + 600),
            ]
            .into(),
            sig: None,
        };
        e.sig = Some(B64.encode(key.sign(e.signed_text().as_bytes()).to_bytes()));
        (e, B64.encode(key.verifying_key().as_bytes()))
    }
    fn token() -> String {
        format!("rwdt_{}", "a".repeat(64))
    }
    async fn fixture(dir: &Path) -> (Service, Arc<Fake>, tokio::task::JoinHandle<()>) {
        let (ent, key) = signed("account-a", "device-a");
        let f = Arc::new(Fake {
            approved: AtomicBool::new(false),
            revoked: AtomicBool::new(false),
            polls: AtomicUsize::new(0),
            ent,
        });
        let app = Router::new()
            .route("/v1/device/start",post(|| async { Json(json!({"device_code":"private-device-code", "user_code":"ABCD-EFGH",
                "verification_uri_complete":"http://localhost:8000/link.html?code=ABCD-EFGH", "expires_in":600,"interval":5})) }))
            .route("/v1/device/token",post(|State(f):State<Arc<Fake>>,Json(b):Json<Value>| async move {
                assert_eq!(b["device_code"],"private-device-code");
                f.polls.fetch_add(1,Ordering::SeqCst);
                if f.approved.load(Ordering::SeqCst) {
                    (StatusCode::OK,Json(json!({"device_token":token(),"account_id":"account-a","device_id":"device-a"})))
                } else { (StatusCode::BAD_REQUEST,Json(json!({"error":"authorization_pending"}))) }
            }))
            .route("/v1/entitlement",get(|State(f):State<Arc<Fake>>,h:axum::http::HeaderMap| async move {
                assert_eq!(h["authorization"],format!("Bearer {}",token()));
                if f.revoked.load(Ordering::SeqCst) { (StatusCode::UNAUTHORIZED,Json(json!({"error":"revoked"}))) }
                else { (StatusCode::OK,Json(json!(f.ent))) }
            })).with_state(f.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let service = Service::new(
            &format!("http://{}", listener.local_addr().unwrap()),
            dir.join("config"),
            dir.join("data/entitlement.json"),
            Some(key),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (service, f, task)
    }
    async fn poll_now(s: &Service) {
        if let Some(p) = &mut s.state.lock().await.pending {
            p.next_poll = Instant::now();
        }
        s.tick().await;
    }

    #[tokio::test]
    async fn approval_persists_only_locally_survives_restart_and_revocation_locks() {
        let d = tempfile::tempdir().unwrap();
        let (s, f, task) = fixture(d.path()).await;
        s.start().await.unwrap();
        let pending = s.view().await.to_string();
        assert!(pending.contains("ABCD-EFGH"));
        assert!(!pending.contains("private-device-code"));
        s.tick().await;
        assert_eq!(f.polls.load(Ordering::SeqCst), 0, "respect poll interval");
        poll_now(&s).await;
        assert_eq!(s.view().await["status"], "pending");
        f.approved.store(true, Ordering::SeqCst);
        poll_now(&s).await;
        let v = s.view().await;
        assert_eq!(v["status"], "linked");
        assert_eq!(v["threat_unlocked"], true);
        assert!(!v.to_string().contains(&token()));
        assert!(!v.to_string().contains("device_token"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(s.dir.join("device-token"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let restarted = Service::new(
            s.api.as_ref().unwrap().as_str(),
            s.dir.clone(),
            s.entitlement.clone(),
            s.key.clone(),
        );
        assert_eq!(restarted.view().await["threat_unlocked"], true);
        assert!(
            restarted.start().await.is_err(),
            "cannot overwrite linked identity"
        );
        f.revoked.store(true, Ordering::SeqCst);
        restarted.tick().await;
        assert_eq!(restarted.view().await["status"], "signed_out");
        assert!(!s.entitlement.exists());
        assert!(s.token().is_none());
        task.abort();
    }

    #[tokio::test]
    async fn expiry_cancel_and_invalid_claim_never_create_credentials() {
        let d = tempfile::tempdir().unwrap();
        let (s, f, task) = fixture(d.path()).await;
        s.start().await.unwrap();
        s.state.lock().await.pending.as_mut().unwrap().expires = Instant::now();
        s.tick().await;
        assert_eq!(s.view().await["status"], "signed_out");
        assert_eq!(f.polls.load(Ordering::SeqCst), 0);
        assert!(
            s.save_claim(&json!({"device_token":"bad","account_id":"a","device_id":"d"}))
                .is_err()
        );
        assert!(s.token().is_none());
        s.start().await.unwrap();
        let s = Arc::new(s);
        assert_eq!(disconnect(Extension(s.clone())).await.0, StatusCode::OK);
        f.approved.store(true, Ordering::SeqCst);
        poll_now(&s).await;
        assert!(s.token().is_none());
        task.abort();
    }

    #[tokio::test]
    async fn signatures_are_bound_to_identity_and_untrusted_plan_does_not_unlock() {
        let d = tempfile::tempdir().unwrap();
        let (s, f, task) = fixture(d.path()).await;
        let id = Identity {
            api: s.api.as_ref().unwrap().to_string(),
            account_id: "account-a".into(),
            device_id: "device-a".into(),
        };
        assert!(s.matches(&f.ent, &id));
        for (a, b) in [("different", "device-a"), ("account-a", "different")] {
            assert!(!s.matches(&signed(a, b).0, &id));
        }
        let mut forged = f.ent.clone();
        forged.wardens.insert("threat".into(), u64::MAX);
        assert!(!s.matches(&forged, &id));
        let missing_key = Service::new(&id.api, s.dir.clone(), s.entitlement.clone(), None);
        assert!(!missing_key.matches(&f.ent, &id));
        s.save_claim(
            &json!({"device_token":token(),"account_id":"account-a","device_id":"device-a"}),
        )
        .unwrap();
        private_write(
            &s.entitlement,
            &serde_json::to_vec(&signed("foreign", "device-a").0).unwrap(),
        )
        .unwrap();
        assert_eq!(s.view().await["threat_unlocked"], false);
        task.abort();
    }

    #[test]
    fn network_targets_are_explicit_and_no_credentials_go_in_urls() {
        for url in [
            "http://evil.test",
            "https://user:pass@example.org",
            "https://example.org/path",
            "https://example.org/?key=abc",
            "file:///tmp/test",
        ] {
            assert!(api_url(url).is_none(), "{url}");
        }
        let api = api_url("https://api.ramwarden.com").unwrap();
        assert!(approval_url(
            &Url::parse("https://ramwarden.com/link.html?code=A").unwrap(),
            &api
        ));
        for link in [
            "http://localhost/link.html",
            "javascript:alert(1)",
            "https://u:p@example.org/link.html",
            "https://example.org/other",
        ] {
            assert!(!approval_url(&Url::parse(link).unwrap(), &api), "{link}");
        }
    }

    #[tokio::test]
    async fn offline_refresh_preserves_only_an_already_verified_plan() {
        let d = tempfile::tempdir().unwrap();
        let (s, f, task) = fixture(d.path()).await;
        s.start().await.unwrap();
        f.approved.store(true, Ordering::SeqCst);
        poll_now(&s).await;
        task.abort();
        let _ = task.await;
        s.state.lock().await.refresh_after = None;
        s.tick().await;
        assert_eq!(s.view().await["threat_unlocked"], true);
        assert!(
            s.view().await["message"]
                .as_str()
                .unwrap()
                .contains("offline")
        );
        s.clear().unwrap();
        assert_eq!(s.view().await["status"], "signed_out");
    }

    #[tokio::test]
    async fn legacy_credential_has_a_visible_disconnect_path() {
        let d = tempfile::tempdir().unwrap();
        let s = Arc::new(Service::new(
            "http://127.0.0.1:1",
            d.path().join("config"),
            d.path().join("entitlement.json"),
            None,
        ));
        private_write(&s.dir.join("device-token"), token().as_bytes()).unwrap();
        assert_eq!(s.view().await["status"], "unverified_device");
        assert!(s.start().await.is_err());
        assert_eq!(disconnect(Extension(s.clone())).await.0, StatusCode::OK);
        assert_eq!(s.view().await["status"], "signed_out");
    }
}
