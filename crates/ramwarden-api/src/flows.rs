//! The website's and the daemon's flows end to end, against the router: wallets
//! that sign the way MetaMask and Phantom do, a mailer that keeps its codes,
//! contract logs as the watcher reads them, and a fake Stripe. The site
//! wardens' support chat has its own file, `support_flows.rs`, on this harness.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use ramwarden_ai::secret::Secret;
use ramwarden_cloud::model::{BillingUpdate, Interval, Platform, Tier, chain_key};
use ramwarden_cloud::store::{MemoryStore, Store};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::accounts::{Cloud, Site};
use crate::billing::Billing;
use crate::catalog::Catalog;
use crate::checkout::tests::{base, cancelled_log, trial_log};
use crate::checkout::{BASE_SEPOLIA, BaseChain, Plans, decode_logs, record};
use crate::entitlement::{EntitlementSigner, Record, verify};
use crate::limits::Limits;
use crate::mail::MemoryMailer;
use crate::routes::{AppState, router};
use crate::stripe::Stripe;
use crate::stripe::tests::{sign, subscription_event};
use crate::upstream::Upstream;
use crate::wallet::iso8601;
use crate::wallet::testing::{EvmWallet, SolWallet};

pub(crate) const NOW: u64 = 1_793_000_000;
const DAY: u64 = 86_400;
const SEED: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
pub(crate) const ORIGIN: &str = "https://ramwarden.app";
const WHSEC: &str = "whsec_test_secret";
pub(crate) const STAFF: &str = "rwstaff_test_token_0123456789";

pub(crate) struct App {
    pub(crate) router: axum::Router,
    pub(crate) store: Arc<MemoryStore>,
    pub(crate) mailer: Arc<MemoryMailer>,
}

#[derive(Clone, Copy)]
pub(crate) enum Auth<'a> {
    None,
    Cookie(&'a str),
    Bearer(&'a str),
}

struct Opts {
    downloads: Option<PathBuf>,
    stripe: Option<String>,
}

pub(crate) fn app() -> App {
    app_with(Opts { downloads: None, stripe: None })
}

fn app_with(o: Opts) -> App {
    let store = Arc::new(MemoryStore::new());
    let mailer = Arc::new(MemoryMailer::default());
    let cloud = Cloud {
        store: store.clone(),
        plans: Arc::new(Plans::built_in()),
        site: Site { domain: "ramwarden.app".into(), origin: ORIGIN.into() },
        billing: Billing {
            base: Some(Arc::new(BaseChain::new(base()))),
            stripe: o.stripe.map(|url| Stripe::with_base(Secret::new("sk_test_123"), Secret::new(WHSEC), &url)),
        },
        signer: Some(EntitlementSigner::from_base64(SEED).unwrap()),
        mailer: Some(mailer.clone()),
        downloads_dir: o.downloads,
        siwe_chain_id: BASE_SEPOLIA,
        public_url: "https://api.ramwarden.app".into(),
        releases_url: crate::install::DEFAULT_RELEASES_URL.into(),
        support: crate::support::SupportDesk::new(Some(crate::config::hash_token(STAFF))),
    };
    let state = AppState {
        tokens: Default::default(),
        catalog: Catalog::built_in(),
        limits: Limits::new(100, 100, u64::MAX),
        upstream: Upstream::new("http://127.0.0.1:9", "jev-latest", Secret::new("k-0123456789"), Duration::from_secs(1))
            .unwrap(),
        max_state_bytes: 1000,
        max_questions: 4,
        now: || NOW,
        cloud: Some(cloud),
    };
    App { router: router(Arc::new(state)), store, mailer }
}

fn session_from(h: &HeaderMap) -> Option<String> {
    let c = h.get(header::SET_COOKIE)?.to_str().ok()?;
    let v = c.split(';').next()?.strip_prefix("rw_session=")?;
    (!v.is_empty()).then(|| v.to_string())
}

impl App {
    pub(crate) async fn raw(&self, method: &str, uri: &str, auth: Auth<'_>, origin: &str, body: Option<Value>) -> (StatusCode, Value, HeaderMap) {
        let mut r = Request::builder().method(method).uri(uri).header(header::ORIGIN, origin);
        match auth {
            Auth::None => {}
            Auth::Cookie(t) => r = r.header(header::COOKIE, format!("theme=dark; rw_session={t}")),
            Auth::Bearer(t) => r = r.header(header::AUTHORIZATION, format!("Bearer {t}")),
        }
        let req = match body {
            Some(b) => r.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
            None => r.body(Body::empty()),
        }
        .unwrap();
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), headers)
    }

    pub(crate) async fn call(&self, method: &str, uri: &str, auth: Auth<'_>, body: Option<Value>) -> (StatusCode, Value) {
        let (s, v, _) = self.raw(method, uri, auth, ORIGIN, body).await;
        (s, v)
    }

    /// Wallet sign-in as the website does it. Returns the reply and the
    /// session cookie.
    async fn sign_in_evm(&self, w: &EvmWallet, code: Option<&str>) -> (StatusCode, Value, Option<String>) {
        let (s, ch) = self
            .call("POST", "/v1/auth/challenge", Auth::None, Some(json!({"chain": "evm", "address": w.address(), "user_code": code})))
            .await;
        assert_eq!(s, StatusCode::OK, "{ch}");
        let m = ch["message"].as_str().unwrap();
        let body = json!({"chain": "evm", "address": w.address(), "message": m, "signature": w.personal_sign(m)});
        let (s, v, h) = self.raw("POST", "/v1/auth/verify", Auth::None, ORIGIN, Some(body)).await;
        (s, v, session_from(&h))
    }

    pub(crate) async fn web(&self, w: &EvmWallet) -> (String, String) {
        let (s, v, cookie) = self.sign_in_evm(w, None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        (cookie.unwrap(), v["account"]["id"].as_str().unwrap().to_string())
    }

    async fn start_device(&self) -> (String, String) {
        let (_, start) = self
            .call("POST", "/v1/device/start", Auth::None, Some(json!({"device_name": "ethan-desktop", "platform": "linux"})))
            .await;
        (start["device_code"].as_str().unwrap().into(), start["user_code"].as_str().unwrap().into())
    }

    /// The daemon's whole sign-in: start, approve with a wallet, claim.
    async fn sign_in_device(&self, w: &EvmWallet) -> (String, String, String) {
        let (dc, code) = self.start_device().await;
        let (s, v, cookie) = self.sign_in_evm(w, Some(&code)).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let (s, tok) = self.call("POST", "/v1/device/token", Auth::None, Some(json!({"device_code": dc}))).await;
        assert_eq!(s, StatusCode::OK, "{tok}");
        (tok["device_token"].as_str().unwrap().into(), cookie.unwrap(), v["account"]["id"].as_str().unwrap().into())
    }

    async fn chain(&self, logs: &[Value]) {
        let plans = Plans::built_in();
        for seen in decode_logs(logs, &base().checkout) {
            assert!(record(self.store.as_ref(), &plans, &seen).await.unwrap());
        }
    }

    async fn entitlement(&self, device_token: &str) -> Record {
        let (s, ent) = self.call("GET", "/v1/entitlement", Auth::Bearer(device_token), None).await;
        assert_eq!(s, StatusCode::OK, "{ent}");
        let rec: Record = serde_json::from_value(ent).unwrap();
        assert!(verify(&rec, &EntitlementSigner::from_base64(SEED).unwrap().public_key()));
        rec
    }
}

// ------------------------------------------------------------- sign-in

#[tokio::test]
async fn a_daemon_signs_in_through_the_link_page_with_metamask() {
    let a = app();
    let (dc, code) = a.start_device().await;
    let (s, poll) = a.call("POST", "/v1/device/token", Auth::None, Some(json!({"device_code": dc}))).await;
    assert_eq!((s, poll["error"].as_str()), (StatusCode::BAD_REQUEST, Some("authorization_pending")));

    // The link page shows the device first, from a code typed in lowercase.
    let (s, look) = a.call("GET", &format!("/v1/device/lookup?user_code={}", code.to_lowercase()), Auth::None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((look["device_name"].as_str(), look["platform"].as_str()), (Some("ethan-desktop"), Some("linux")));

    let w = EvmWallet::new(3);
    let (s, v, cookie) = a.sign_in_evm(&w, Some(&code)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v.get("session").is_none(), "the session is a cookie, never in the body");
    let cookie = cookie.expect("rw_session cookie set");
    assert_eq!(v["linked_device"]["name"], "ethan-desktop");
    let acct = &v["account"];
    assert_eq!((acct["tier"].clone(), acct["status"].clone()), (Value::Null, Value::Null), "nothing bought yet");
    assert_eq!(acct["identity"]["address"], w.address(), "shown checksummed");
    assert_eq!((acct["device_limit"].as_u64(), acct["trial_available"].as_bool()), (Some(1), Some(true)), "Lite until a plan starts");

    let (s, tok) = a.call("POST", "/v1/device/token", Auth::None, Some(json!({"device_code": dc}))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(tok["device_token"].as_str().unwrap().starts_with("rwdt_"));
    let (status, summary) = a.call("GET", "/v1/device/account", Auth::Bearer(tok["device_token"].as_str().unwrap()), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summary, json!({"plan":"lite","device_limit":1,"devices_used":1}));
    assert_ne!(a.call("GET", "/v1/device/account", Auth::Cookie(&cookie), None).await.0, StatusCode::OK);

    let (s, again) = a.call("POST", "/v1/device/token", Auth::None, Some(json!({"device_code": dc}))).await;
    assert_eq!((s, again["error"].as_str()), (StatusCode::BAD_REQUEST, Some("expired_token")), "claimed once");

    let (_, acct) = a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await;
    assert_eq!(acct["devices"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn phantom_signs_in_and_an_agent_can_ask_for_a_bearer() {
    let a = app();
    let w = SolWallet::new(4);
    let mut ids = vec![];
    for want in [None, Some("bearer")] {
        let (_, ch) = a.call("POST", "/v1/auth/challenge", Auth::None, Some(json!({"chain": "solana", "address": w.address()}))).await;
        let m = ch["message"].as_str().unwrap();
        assert!(m.contains("with your Solana account:"));
        let body = json!({"chain": "solana", "address": w.address(), "message": m, "signature": w.sign_message(m), "session": want});
        let (s, v, h) = a.raw("POST", "/v1/auth/verify", Auth::None, ORIGIN, Some(body)).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        if want.is_some() {
            assert!(session_from(&h).is_none());
            let t = v["session"].as_str().unwrap();
            assert_eq!(a.call("GET", "/v1/account", Auth::Bearer(t), None).await.0, StatusCode::OK);
        }
        ids.push(v["account"]["id"].clone());
    }
    assert_eq!(ids[0], ids[1], "one wallet, one account");
}

#[tokio::test]
async fn forged_replayed_or_foreign_signatures_are_refused() {
    let a = app();
    let (w, thief) = (EvmWallet::new(5), EvmWallet::new(6));
    let challenge = |addr: String| {
        let a = &a;
        async move { a.call("POST", "/v1/auth/challenge", Auth::None, Some(json!({"chain": "evm", "address": addr}))).await.1 }
    };
    let verify = |body: Value| {
        let a = &a;
        async move { a.call("POST", "/v1/auth/verify", Auth::None, Some(body)).await.0 }
    };
    let m = challenge(w.address()).await["message"].as_str().unwrap().to_string();
    assert_eq!(verify(json!({"chain": "evm", "address": w.address(), "message": m, "signature": thief.personal_sign(&m)})).await, StatusCode::UNAUTHORIZED);
    // The right signature, but the failure used the challenge up.
    assert_eq!(verify(json!({"chain": "evm", "address": w.address(), "message": m, "signature": w.personal_sign(&m)})).await, StatusCode::UNAUTHORIZED);
    // A message the server did not write, quoting a live nonce.
    let m = challenge(w.address()).await["message"].as_str().unwrap().to_string();
    let edited = m.replace("ramwarden.app wants", "evil.example wants");
    assert_eq!(verify(json!({"chain": "evm", "address": w.address(), "message": edited, "signature": w.personal_sign(&edited)})).await, StatusCode::UNAUTHORIZED);
    // A challenge for one wallet, claimed by another.
    let m = challenge(w.address()).await["message"].as_str().unwrap().to_string();
    assert_eq!(verify(json!({"chain": "evm", "address": thief.address(), "message": m, "signature": thief.personal_sign(&m)})).await, StatusCode::UNAUTHORIZED);
    // Unknown device codes are refused before anything is signed.
    let (s, _) = a
        .call("POST", "/v1/auth/challenge", Auth::None, Some(json!({"chain": "evm", "address": w.address(), "user_code": "BCDF-GHJK"})))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(a.call("GET", "/v1/account", Auth::None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(a.call("GET", "/v1/account", Auth::Cookie("rwse_nope"), None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(a.call("GET", "/v1/account", Auth::Bearer("rwdt_device_token_is_not_a_session"), None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn email_sign_in_mails_a_single_use_code_and_links_a_device() {
    let a = app();
    let (dc, code) = a.start_device().await;
    let email = "Ethan@Example.com";
    let (s, v) = a.call("POST", "/v1/auth/email/start", Auth::None, Some(json!({"email": email, "user_code": code}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let sent = a.mailer.last_code("ethan@example.com").expect("mailed to the lowercased address");
    let (s, _) = a.call("POST", "/v1/auth/email/start", Auth::None, Some(json!({"email": email}))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "one code a minute");

    let wrong = if sent == "000000" { "111111" } else { "000000" };
    let (s, _) = a.call("POST", "/v1/auth/email/verify", Auth::None, Some(json!({"email": email, "code": wrong}))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v, h) = a.raw("POST", "/v1/auth/email/verify", Auth::None, ORIGIN, Some(json!({"email": email, "code": sent}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["account"]["identity"], json!({"kind": "email", "email": "ethan@example.com"}));
    assert_eq!(v["linked_device"]["name"], "ethan-desktop");
    assert!(session_from(&h).is_some());
    let (s, _) = a.call("POST", "/v1/auth/email/verify", Auth::None, Some(json!({"email": email, "code": sent}))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "used once");

    let (s, _) = a.call("POST", "/v1/device/token", Auth::None, Some(json!({"device_code": dc}))).await;
    assert_eq!(s, StatusCode::OK);
    for bad in ["nope", "a@b", "x@y.z\r\nBcc: q@r.s"] {
        let (s, _) = a.call("POST", "/v1/auth/email/start", Auth::None, Some(json!({"email": bad}))).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{bad:?}");
    }
}

#[tokio::test]
async fn logout_ends_the_session_and_clears_the_cookie() {
    let a = app();
    let (cookie, _) = a.web(&EvmWallet::new(11)).await;
    let (s, _, h) = a.raw("POST", "/v1/auth/logout", Auth::Cookie(&cookie), ORIGIN, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=0"));
    assert_eq!(a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_sixth_device_is_refused_on_the_link_page() {
    let a = app();
    let w = EvmWallet::new(12);
    let (cookie, id) = a.web(&w).await;
    // Plus covers 3 devices and Pro 5 (user decision, 2026-11-02).
    let plus = BillingUpdate { tier: Some(Tier::Ram), interval: Some(Interval::Month), paid_until: Some(NOW + 60 * DAY), ..Default::default() };
    a.store.update_billing(&id, &plus).await.unwrap();
    for i in 0..3 {
        a.store.register_device(&id, &format!("pc{i}"), Platform::Linux, NOW).await.unwrap().unwrap();
    }
    let (_, code) = a.start_device().await;
    let (s, v, _) = a.sign_in_evm(&w, Some(&code)).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v["error"].as_str().unwrap().contains("Plus covers 3 devices"), "{v}");
    let (_, acct) = a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await;
    assert_eq!((acct["plan"].as_str(), acct["device_limit"].as_u64(), acct["pet"].as_bool()), (Some("plus"), Some(3), Some(true)));

    let pro = BillingUpdate { tier: Some(Tier::Pro), ..Default::default() };
    a.store.update_billing(&id, &pro).await.unwrap();
    for i in 3..5 {
        a.store.register_device(&id, &format!("pc{i}"), Platform::Linux, NOW).await.unwrap().unwrap();
    }
    let (dc, code) = a.start_device().await;
    let (s, v, _) = a.sign_in_evm(&w, Some(&code)).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v["error"].as_str().unwrap().contains("5 devices"));
    let (s, _) = a.call("POST", "/v1/device/token", Auth::None, Some(json!({"device_code": dc}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "not approved");

    // Removing one frees the slot.
    let (_, acct) = a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await;
    let dev = acct["devices"][0]["id"].as_str().unwrap();
    assert_eq!(a.call("DELETE", &format!("/v1/account/devices/{dev}"), Auth::Cookie(&cookie), None).await.0, StatusCode::OK);
    let (dc, code) = a.start_device().await;
    assert_eq!(a.sign_in_evm(&w, Some(&code)).await.0, StatusCode::OK);
    assert_eq!(a.call("POST", "/v1/device/token", Auth::None, Some(json!({"device_code": dc}))).await.0, StatusCode::OK);
}

// ------------------------------------------------------------- account

#[tokio::test]
async fn prefs_follow_the_account_and_only_the_site_may_change_them() {
    let a = app();
    let (cookie, _) = a.web(&EvmWallet::new(13)).await;
    let prefs = json!({"wardens": {"ram": {"color": "#c0182a"}, "threat": {"color": "#7a3cff"}}, "pet_comments": false});
    let (s, v) = a.call("PUT", "/v1/account/prefs", Auth::Cookie(&cookie), Some(prefs.clone())).await;
    assert_eq!((s, &v["prefs"]), (StatusCode::OK, &prefs));
    assert_eq!(a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await.1["prefs"], prefs);

    let (s, _, _) = a.raw("PUT", "/v1/account/prefs", Auth::Cookie(&cookie), "https://evil.example", Some(json!({}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "a cookie sent from another origin changes nothing");
    let big = json!({"x": "y".repeat(5000)});
    assert_eq!(a.call("PUT", "/v1/account/prefs", Auth::Cookie(&cookie), Some(big)).await.0, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(a.call("PUT", "/v1/account/prefs", Auth::Cookie(&cookie), Some(json!([1]))).await.0, StatusCode::UNPROCESSABLE_ENTITY);
}

// ------------------------------------------------------------- Base

#[tokio::test]
async fn a_base_trial_unlocks_pro_and_the_entitlement_says_so() {
    let a = app();
    let w = EvmWallet::new(7);
    let (device_token, cookie, account_id) = a.sign_in_device(&w).await;

    let (_, cfg) = a.call("GET", "/v1/checkout/config", Auth::None, None).await;
    assert_eq!(cfg["base"]["chain_id"], BASE_SEPOLIA);
    let prices: Vec<(u64, &str, u64)> = cfg["plans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_u64().unwrap(), p["tier"].as_str().unwrap(), p["usd"].as_u64().unwrap()))
        .collect();
    assert_eq!(prices, vec![(1, "ram", 4), (2, "pro", 5), (3, "ram", 20), (4, "pro", 24)]);
    assert_eq!(cfg["stripe"], false);

    // Before the trial: no hosted decisions, an empty entitlement.
    assert_eq!(a.call("GET", "/v1/models", Auth::Bearer(&device_token), None).await.0, StatusCode::PAYMENT_REQUIRED);
    let rec = a.entitlement(&device_token).await;
    assert_eq!((rec.tier.as_str(), rec.status.as_str(), rec.wardens.len()), ("none", "none", 0));

    let trials: Vec<u64> = cfg["plans"].as_array().unwrap().iter().map(|p| p["trial_days"].as_u64().unwrap()).collect();
    assert_eq!(trials, vec![14, 0, 14, 0], "RamWarden has the trial, Pro doesn't");

    let (s, co) = a.call("POST", "/v1/checkout/base", Auth::Cookie(&cookie), Some(json!({"plan": 1, "payer": w.address()}))).await;
    assert_eq!(s, StatusCode::OK, "{co}");
    assert_eq!((co["trial"].as_bool(), co["approval_usd"].as_u64()), (Some(true), Some(48)));
    assert_eq!(co["transactions"].as_array().unwrap().len(), 2, "approve one year, then startTrial");
    let pid = co["payment_id"].as_str().unwrap().to_string();
    let (_, p) = a.call("GET", &format!("/v1/checkout/payments/{pid}"), Auth::Cookie(&cookie), None).await;
    assert_eq!(p["status"], "pending");

    let mut r = [0u8; 32];
    r.copy_from_slice(&hex::decode(&pid).unwrap());
    let trial_end = NOW + 14 * DAY;
    a.chain(&[trial_log(&base().checkout, chain_key(&account_id), 1, trial_end, r)]).await;

    let (_, p) = a.call("GET", &format!("/v1/checkout/payments/{pid}"), Auth::Cookie(&cookie), None).await;
    assert_eq!((p["status"].as_str(), p["account_status"].as_str()), (Some("confirmed"), Some("trial")));
    let (_, acct) = a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await;
    assert_eq!((acct["tier"].as_str(), acct["interval"].as_str(), acct["status"].as_str()), (Some("ram"), Some("month"), Some("trial")));
    assert_eq!(acct["trial_ends_at"], iso8601(trial_end));
    assert_eq!(acct["billing"]["method"], "base");
    assert_eq!(acct["trial_available"], false);

    let rec = a.entitlement(&device_token).await;
    assert_eq!((rec.tier.as_str(), rec.status.as_str()), ("ram", "trial"));
    let grace_end = trial_end + ramwarden_cloud::model::GRACE_SECONDS;
    assert_eq!(rec.wardens["ram"], grace_end);
    assert!(!rec.wardens.contains_key("threat"), "ThreatWarden is Pro's");
    assert_eq!(a.call("GET", "/v1/models", Auth::Bearer(&device_token), None).await.0, StatusCode::OK);

    // One subscription at a time.
    let (s, _) = a.call("POST", "/v1/checkout/base", Auth::Cookie(&cookie), Some(json!({"plan": 1}))).await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Cancel: the paying wallet sends cancel(account); service runs on.
    let (s, c) = a.call("POST", "/v1/checkout/cancel", Auth::Cookie(&cookie), None).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["transactions"][0]["to"], base().checkout_hex());
    a.chain(&[cancelled_log(&base().checkout, chain_key(&account_id))]).await;
    let (_, acct) = a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await;
    assert_eq!(acct["status"], "cancelled");
    assert_eq!(acct["entitlement_until"], iso8601(trial_end), "no grace after a cancel");
    assert_eq!(a.call("POST", "/v1/checkout/cancel", Auth::Cookie(&cookie), None).await.0, StatusCode::CONFLICT);

    // Another account cannot read the payment.
    let (other, _) = a.web(&EvmWallet::new(8)).await;
    assert_eq!(a.call("GET", &format!("/v1/checkout/payments/{pid}"), Auth::Cookie(&other), None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pro_on_base_is_charged_at_once_and_keeps_the_trial_unused() {
    let a = app();
    let w = EvmWallet::new(16);
    let (device_token, cookie, account_id) = a.sign_in_device(&w).await;
    let (s, co) = a.call("POST", "/v1/checkout/base", Auth::Cookie(&cookie), Some(json!({"plan": 2, "payer": w.address()}))).await;
    assert_eq!(s, StatusCode::OK, "{co}");
    assert_eq!((co["trial"].as_bool(), co["approval_usd"].as_u64()), (Some(false), Some(60)));
    let mut r = [0u8; 32];
    r.copy_from_slice(&hex::decode(co["payment_id"].as_str().unwrap()).unwrap());

    // The contract's start with no trial, then the first charge.
    let paid = NOW + 60 * DAY;
    let key = chain_key(&account_id);
    a.chain(&[trial_log(&base().checkout, key, 2, 0, r), crate::checkout::tests::charged_log(&base().checkout, key, 2, paid)]).await;
    let (_, acct) = a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await;
    assert_eq!((acct["tier"].as_str(), acct["status"].as_str()), (Some("pro"), Some("active")));
    assert_eq!((acct["trial_ends_at"].clone(), acct["paid_until"].clone()), (Value::Null, json!(iso8601(paid))));
    assert_eq!(acct["trial_available"], true, "the Plus trial is still theirs");
    let rec = a.entitlement(&device_token).await;
    assert_eq!((rec.tier.as_str(), rec.status.as_str()), ("pro", "active"));
    assert!(rec.wardens.contains_key("threat"));
}

#[tokio::test]
async fn lite_is_a_free_account_on_one_device_with_no_hosted_ai() {
    let a = app();
    let w = EvmWallet::new(17);
    let (tok, cookie, _) = a.sign_in_device(&w).await;
    let (_, acct) = a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await;
    assert_eq!((acct["plan"].as_str(), acct["device_limit"].as_u64(), acct["pet"].as_bool()), (Some("lite"), Some(1), Some(false)));
    assert_eq!(a.call("GET", "/v1/models", Auth::Bearer(&tok), None).await.0, StatusCode::PAYMENT_REQUIRED, "no Jev on Lite");

    // A second device is refused on the link page and for the install line.
    let (_, code) = a.start_device().await;
    let (s, v, _) = a.sign_in_evm(&w, Some(&code)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["error"].as_str().unwrap().contains("Lite covers 1 device"), "{v}");
    assert_eq!(a.call("POST", "/v1/install/link", Auth::Cookie(&cookie), None).await.0, StatusCode::CONFLICT);

    // With no device yet, Lite gets its install line.
    let (fresh, _) = a.web(&EvmWallet::new(18)).await;
    assert_eq!(a.call("POST", "/v1/install/link", Auth::Cookie(&fresh), None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn plus_upgrades_to_pro_on_base_in_one_wallet_call() {
    let a = app();
    let w = EvmWallet::new(19);
    let (cookie, account_id) = a.web(&w).await;
    let key = chain_key(&account_id);
    let (_, co) = a.call("POST", "/v1/checkout/base", Auth::Cookie(&cookie), Some(json!({"plan": 1}))).await;
    let mut r = [0u8; 32];
    r.copy_from_slice(&hex::decode(co["payment_id"].as_str().unwrap()).unwrap());
    a.chain(&[trial_log(&base().checkout, key, 1, NOW + 14 * DAY, r)]).await;

    // Down or sideways is refused; up returns changePlan for the paying wallet.
    let (s, _) = a.call("POST", "/v1/checkout/base", Auth::Cookie(&cookie), Some(json!({"plan": 1}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, up) = a.call("POST", "/v1/checkout/base", Auth::Cookie(&cookie), Some(json!({"plan": 2}))).await;
    assert_eq!(s, StatusCode::OK, "{up}");
    assert_eq!((up["upgrade"].as_bool(), up["trial"].as_bool()), (Some(true), Some(false)));
    let txs = up["transactions"].as_array().unwrap();
    let selector = hex::encode(crate::checkout::selector("changePlan(bytes32,uint32,uint256,bytes32)"));
    assert!(txs.last().unwrap()["data"].as_str().unwrap()[2..].starts_with(&selector));
    assert!(a.call("POST", "/v1/checkout/stripe", Auth::Cookie(&cookie), Some(json!({"plan": 2}))).await.0 != StatusCode::OK);

    let mut r = [0u8; 32];
    r.copy_from_slice(&hex::decode(up["payment_id"].as_str().unwrap()).unwrap());
    a.chain(&[trial_log(&base().checkout, key, 2, 0, r), crate::checkout::tests::charged_log(&base().checkout, key, 2, NOW + 60 * DAY)]).await;
    let (_, acct) = a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await;
    assert_eq!((acct["plan"].as_str(), acct["status"].as_str()), (Some("pro"), Some("active")), "the trial ended with the upgrade");
    assert_eq!(acct["trial_ends_at"], Value::Null);
    assert_eq!(acct["device_limit"], 5);
    let (s, _) = a.call("POST", "/v1/checkout/base", Auth::Cookie(&cookie), Some(json!({"plan": 1}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "Pro to Plus is cancel, then subscribe");
}

// ------------------------------------------------------------- Stripe

type Seen = Arc<Mutex<Vec<(String, String)>>>;
type PaymentStoreProbe = Arc<Mutex<Option<Arc<MemoryStore>>>>;

/// Answers Checkout Session creation and subscription updates like Stripe.
async fn fake_stripe() -> (String, Seen, PaymentStoreProbe) {
    use axum::routing::post;
    let seen: Seen = Arc::default();
    let probe: PaymentStoreProbe = Arc::default();
    let check_store = probe.clone();
    let s1 = seen.clone();
    let s2 = seen.clone();
    let app = axum::Router::new()
        .route(
            "/v1/checkout/sessions",
            post(move |body: String| async move {
                let store = check_store.lock().unwrap().clone().expect("payment-store probe");
                let pid = body.split('&').find_map(|part| part.strip_prefix("subscription_data%5Bmetadata%5D%5Bpayment_id%5D=")).expect("payment metadata");
                assert!(store.payment(pid).await.unwrap().is_some(), "payment must exist before Stripe is called");
                s1.lock().unwrap().push(("checkout".into(), body));
                Json(json!({"id": "cs_test_1", "url": "https://checkout.stripe.com/c/pay/cs_test_1"}))
            }),
        )
        .route(
            "/v1/subscriptions/{id}",
            post(move |axum::extract::Path(id): axum::extract::Path<String>, body: String| async move {
                s2.lock().unwrap().push((id, body));
                Json(json!({"id": "sub_123"}))
            })
            .get(|| async { Json(json!({"id": "sub_123", "items": {"data": [{"id": "si_1"}]}})) }),
        )
        .route("/v1/prices", post(|| async { Json(json!({"id": "price_pro"})) }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, seen, probe)
}

#[tokio::test]
async fn a_card_trial_starts_from_a_signed_webhook() {
    let (url, seen, probe) = fake_stripe().await;
    let a = app_with(Opts { downloads: None, stripe: Some(url) });
    *probe.lock().unwrap() = Some(a.store.clone());
    let (s, _) = a.call("POST", "/v1/auth/email/start", Auth::None, Some(json!({"email": "card@example.com"}))).await;
    assert_eq!(s, StatusCode::OK);
    let code = a.mailer.last_code("card@example.com").unwrap();
    let (_, v, h) = a.raw("POST", "/v1/auth/email/verify", Auth::None, ORIGIN, Some(json!({"email": "card@example.com", "code": code}))).await;
    let cookie = session_from(&h).unwrap();
    let account_id = v["account"]["id"].as_str().unwrap().to_string();

    let (_, cfg) = a.call("GET", "/v1/checkout/config", Auth::None, None).await;
    assert_eq!((cfg["stripe"].as_bool(), cfg["stripe_test"].as_bool()), (Some(true), Some(true)));

    let (s, co) = a.call("POST", "/v1/checkout/stripe", Auth::Cookie(&cookie), Some(json!({"plan": 3}))).await;
    assert_eq!(s, StatusCode::OK, "{co}");
    assert_eq!(co["url"], "https://checkout.stripe.com/c/pay/cs_test_1");
    let pid = co["payment_id"].as_str().unwrap().to_string();
    let form = seen.lock().unwrap()[0].1.clone();
    for part in ["mode=subscription", "unit_amount%5D=2000", "interval%5D=year", "interval_count%5D=1", "trial_period_days%5D=14", "customer_email=card%40example.com"] {
        assert!(form.contains(part), "{part} in {form}");
    }
    assert!(form.contains(&format!("payment_id%5D={pid}")));

    // Stripe calls back. An unsigned or mis-signed event changes nothing.
    let trial_end = NOW + 14 * DAY;
    let event = subscription_event("customer.subscription.created", &account_id, 3, "trialing", trial_end, Some(trial_end));
    let mut event = event;
    event["data"]["object"]["metadata"]["payment_id"] = json!(pid);
    let body = event.to_string();
    let post = |sig: String| {
        let req = Request::post("/v1/stripe/webhook")
            .header("stripe-signature", sig)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.clone()))
            .unwrap();
        a.router.clone().oneshot(req)
    };
    assert_eq!(post(sign("whsec_other", body.as_bytes(), NOW)).await.unwrap().status(), StatusCode::BAD_REQUEST);
    assert_eq!(post(sign(WHSEC, body.as_bytes(), NOW - 3600)).await.unwrap().status(), StatusCode::BAD_REQUEST, "replayed");
    assert_eq!(a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await.1["tier"], Value::Null);
    assert_eq!(post(sign(WHSEC, body.as_bytes(), NOW)).await.unwrap().status(), StatusCode::OK);

    let (_, acct) = a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await;
    assert_eq!((acct["tier"].as_str(), acct["interval"].as_str(), acct["status"].as_str()), (Some("ram"), Some("year"), Some("trial")));
    assert_eq!(acct["billing"]["method"], "card");
    let (_, p) = a.call("GET", &format!("/v1/checkout/payments/{pid}"), Auth::Cookie(&cookie), None).await;
    assert_eq!(p["status"], "confirmed");

    // Upgrading to Pro changes the same subscription, no new checkout page.
    let (s, up) = a.call("POST", "/v1/checkout/stripe", Auth::Cookie(&cookie), Some(json!({"plan": 4}))).await;
    assert_eq!(s, StatusCode::OK, "{up}");
    assert_eq!((up["upgrade"].as_bool(), up["url"].clone()), (Some(true), Value::Null));
    let change = seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(change.0, "sub_123");
    for part in ["items%5B0%5D%5Bid%5D=si_1", "items%5B0%5D%5Bprice%5D=price_pro", "trial_end=now", "metadata%5Bplan%5D=4"] {
        assert!(change.1.contains(part), "{part} in {}", change.1);
    }

    // Cancelling a card subscription stops renewal at Stripe.
    let (s, c) = a.call("POST", "/v1/checkout/cancel", Auth::Cookie(&cookie), None).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["service_until"], iso8601(trial_end));
    let last = seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last, ("sub_123".to_string(), "cancel_at_period_end=true".to_string()));
    assert_eq!(a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await.1["status"], "cancelled");
}

#[tokio::test]
async fn card_checkout_is_closed_without_stripe_keys() {
    let a = app();
    let (cookie, _) = a.web(&EvmWallet::new(14)).await;
    let (s, _) = a.call("POST", "/v1/checkout/stripe", Auth::Cookie(&cookie), Some(json!({"plan": 1}))).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    let req = Request::post("/v1/stripe/webhook").body(Body::from("{}")).unwrap();
    assert_eq!(a.router.clone().oneshot(req).await.unwrap().status(), StatusCode::NOT_FOUND);
}

// ------------------------------------------------------------- downloads

#[tokio::test]
async fn threatwarden_downloads_need_pro_in_good_standing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("threat")).unwrap();
    std::fs::write(dir.path().join("threat/threatwarden-linux-x86_64.tar.gz"), b"installer").unwrap();
    let a = app_with(Opts { downloads: Some(dir.path().to_path_buf()), stripe: None });
    let (tok, cookie, id) = a.sign_in_device(&EvmWallet::new(9)).await;
    let uri = "/v1/downloads/threat/threatwarden-linux-x86_64.tar.gz";

    assert_eq!(a.call("GET", uri, Auth::None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(a.call("GET", uri, Auth::Bearer(&tok), None).await.0, StatusCode::FORBIDDEN, "nothing bought");

    let ram = BillingUpdate { tier: Some(Tier::Ram), paid_until: Some(NOW + 100), ..Default::default() };
    a.store.update_billing(&id, &ram).await.unwrap();
    assert_eq!(a.call("GET", uri, Auth::Bearer(&tok), None).await.0, StatusCode::FORBIDDEN, "RamWarden is not Pro");

    a.store.update_billing(&id, &BillingUpdate { tier: Some(Tier::Pro), ..Default::default() }).await.unwrap();
    let req = Request::get(uri).header(header::AUTHORIZATION, format!("Bearer {tok}")).body(Body::empty()).unwrap();
    let resp = a.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(&to_bytes(resp.into_body(), 1024).await.unwrap()[..], b"installer");

    for bad in ["/v1/downloads/threat/..%2Fsecret", "/v1/downloads/threat/.env", "/v1/downloads/threat/missing.exe"] {
        assert_eq!(a.call("GET", bad, Auth::Bearer(&tok), None).await.0, StatusCode::NOT_FOUND, "{bad}");
    }

    // A removed device loses access at once.
    let (_, acct) = a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await;
    let dev = acct["devices"][0]["id"].as_str().unwrap();
    assert_eq!(a.call("DELETE", &format!("/v1/account/devices/{dev}"), Auth::Cookie(&cookie), None).await.0, StatusCode::OK);
    assert_eq!(a.call("GET", uri, Auth::Bearer(&tok), None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_install_line_links_the_new_device_once() {
    let a = app();
    let w = EvmWallet::new(15);
    let (cookie, id) = a.web(&w).await;
    let trial = BillingUpdate { tier: Some(Tier::Ram), paid_until: Some(NOW + 14 * DAY), trial_ends_at: Some(NOW + 14 * DAY), ..Default::default() };
    a.store.update_billing(&id, &trial).await.unwrap();
    let (s, v) = a.call("POST", "/v1/install/link", Auth::Cookie(&cookie), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["expires_at"], iso8601(NOW + 30 * 60));
    let cmd = v["commands"]["linux"].as_str().unwrap();
    assert!(cmd.starts_with("curl -fsSL https://api.ramwarden.app/v1/install/rwdc_") && cmd.ends_with("/sh | sh"), "{cmd}");
    assert!(v["commands"]["windows"].as_str().unwrap().starts_with("irm "));
    assert!(v["installers"]["macos"].as_str().unwrap().ends_with("/RamWarden.dmg"));
    let token = cmd.split("/v1/install/").nth(1).unwrap().split('/').next().unwrap().to_string();

    // Fetching the script doesn't use the token up.
    let req = Request::get(format!("/v1/install/{token}/sh")).body(Body::empty()).unwrap();
    let resp = a.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let script = String::from_utf8(to_bytes(resp.into_body(), 1 << 16).await.unwrap().to_vec()).unwrap();
    assert!(script.contains(&token) && script.contains("SHA256SUMS"));
    assert_eq!(a.call("GET", "/v1/install/rwdc_nope/sh", Auth::None, None).await.0, StatusCode::NOT_FOUND);

    // The app's first run claims it and names itself.
    let body = json!({"device_code": token, "device_name": "ethan-macbook", "platform": "macos"});
    let (s, tok) = a.call("POST", "/v1/device/token", Auth::None, Some(body.clone())).await;
    assert_eq!(s, StatusCode::OK, "{tok}");
    let (_, acct) = a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await;
    assert_eq!((acct["devices"][0]["name"].as_str(), acct["devices"][0]["platform"].as_str()), (Some("ethan-macbook"), Some("macos")));
    let (s, again) = a.call("POST", "/v1/device/token", Auth::None, Some(body)).await;
    assert_eq!((s, again["error"].as_str()), (StatusCode::BAD_REQUEST, Some("expired_token")), "single use");
}

#[tokio::test]
async fn only_the_site_origin_gets_cors_with_credentials() {
    let a = app();
    let pre = |origin: &'static str| {
        Request::builder()
            .method("OPTIONS")
            .uri("/v1/account/prefs")
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "PUT")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
            .body(Body::empty())
            .unwrap()
    };
    let ok = a.router.clone().oneshot(pre(ORIGIN)).await.unwrap();
    assert_eq!(ok.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], ORIGIN);
    assert_eq!(ok.headers()[header::ACCESS_CONTROL_ALLOW_CREDENTIALS], "true");
    let bad = a.router.clone().oneshot(pre("https://evil.example")).await.unwrap();
    assert_ne!(bad.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).map(|v| v.as_bytes()), Some(&b"https://evil.example"[..]));
}

#[tokio::test]
async fn verified_email_and_wallet_share_one_account_and_subscription() {
    let a = app();
    let (cookie, id) = a.web(&EvmWallet::new(41)).await;
    a.store.update_billing(&id, &BillingUpdate { tier:Some(Tier::Pro), paid_until:Some(NOW+DAY), ..Default::default() }).await.unwrap();
    let email="linked-owner@example.com";
    let (s, _) = a.call("POST","/v1/auth/email/start",Auth::Cookie(&cookie),Some(json!({"email":email,"link":true}))).await;
    assert_eq!(s,StatusCode::OK);
    let code=a.mailer.last_code(email).unwrap();
    let (s, linked, headers)=a.raw("POST","/v1/auth/email/verify",Auth::Cookie(&cookie),ORIGIN,
        Some(json!({"email":email,"code":code,"link":true}))).await;
    assert_eq!(s,StatusCode::OK,"{linked}");
    assert!(headers.get(header::SET_COOKIE).is_none(),"linking keeps the original session");
    assert_eq!(linked["account"]["id"],id);
    assert_eq!(linked["account"]["tier"],"pro");
    assert_eq!(linked["account"]["identities"].as_array().unwrap().len(),2);
    let (s, signed)=a.call("POST","/v1/auth/email/start",Auth::None,Some(json!({"email":email}))).await;
    // A consumed code permits a new, independent normal sign-in.
    assert_eq!(s,StatusCode::OK,"{signed}");
    let code=a.mailer.last_code(email).unwrap();
    let (s, signed)=a.call("POST","/v1/auth/email/verify",Auth::None,Some(json!({"email":email,"code":code}))).await;
    assert_eq!(s,StatusCode::OK,"{signed}");
    assert_eq!(signed["account"]["id"],id);
    assert_eq!(signed["account"]["tier"],"pro");
}

#[tokio::test]
async fn wallet_link_proofs_are_bound_to_the_requesting_account() {
    let a=app();
    let (cookie,id)=a.web(&EvmWallet::new(42)).await;
    let (other_cookie,_)=a.web(&EvmWallet::new(43)).await;
    let wallet=EvmWallet::new(44);
    let body=json!({"chain":"evm","address":wallet.address(),"link":true});
    let (s,_)=a.call("POST","/v1/auth/challenge",Auth::None,Some(body.clone())).await;
    assert_eq!(s,StatusCode::UNAUTHORIZED);
    for (auth,expected) in [(Auth::Cookie(&other_cookie),StatusCode::UNAUTHORIZED),(Auth::Cookie(&cookie),StatusCode::OK)] {
        let (s,ch)=a.call("POST","/v1/auth/challenge",Auth::Cookie(&cookie),Some(body.clone())).await;
        assert_eq!(s,StatusCode::OK,"{ch}");
        let message=ch["message"].as_str().unwrap();
        assert!(message.contains("Link this wallet"));
        let (s,result)=a.call("POST","/v1/auth/verify",auth,
            Some(json!({"chain":"evm","address":wallet.address(),"message":message,"signature":wallet.personal_sign(message),"link":true}))).await;
        assert_eq!(s,expected,"{result}");
    }
    let (_,signed,_)=a.sign_in_evm(&wallet,None).await;
    assert_eq!(signed["account"]["id"],id);
}

#[tokio::test]
async fn linking_refuses_existing_accounts_and_normal_signin_proofs() {
    let a=app();
    let (cookie,id)=a.web(&EvmWallet::new(45)).await;
    let wallet=EvmWallet::new(46);
    let (_,other)=a.web(&wallet).await;
    for link in [false,true] {
        let (_,ch)=a.call("POST","/v1/auth/challenge",Auth::Cookie(&cookie),
            Some(json!({"chain":"evm","address":wallet.address(),"link":link}))).await;
        let message=ch["message"].as_str().unwrap();
        let (s,result)=a.call("POST","/v1/auth/verify",Auth::Cookie(&cookie),
            Some(json!({"chain":"evm","address":wallet.address(),"message":message,"signature":wallet.personal_sign(message),"link":true}))).await;
        assert_eq!(s,if link {StatusCode::CONFLICT} else {StatusCode::UNAUTHORIZED},"{result}");
    }
    assert_ne!(id,other);
    let (_,signed,_)=a.sign_in_evm(&wallet,None).await;
    assert_eq!(signed["account"]["id"],other);
}

#[tokio::test]
async fn diagnostics_are_opt_in_numeric_device_scoped_and_retry_safe() {
    let a=app();let(cookie,id)=a.web(&EvmWallet::new(61)).await;
    let(device,token)=a.store.register_device(&id,"test-mac",Platform::Macos,NOW).await.unwrap().unwrap();
    let day=NOW/86400*86400;
    let payload=json!({"days":[{"day":day,"since":NOW,"counters":{"apps_paused":2},"bytes_recovered":2048}]});
    let(s,_)=a.call("POST","/v1/usage/daily",Auth::Bearer(&token),Some(payload.clone())).await;
    assert_eq!(s,StatusCode::FORBIDDEN);
    let(s,prefs)=a.call("PUT","/v1/account/prefs",Auth::Cookie(&cookie),Some(json!({"share_usage":true,"usage_since":1}))).await;
    assert_eq!(s,StatusCode::OK,"{prefs}");assert_eq!(prefs["prefs"]["usage_since"],NOW);
    let(s,config)=a.call("GET","/v1/usage/config",Auth::Bearer(&token),None).await;
    assert_eq!(s,StatusCode::OK);assert_eq!(config["enabled"],true);assert_eq!(config["device_id"],device.id);
    for count in [2,1] {
        let mut p=payload.clone();p["days"][0]["counters"]["apps_paused"]=json!(count);
        let(s,result)=a.call("POST","/v1/usage/daily",Auth::Bearer(&token),Some(p)).await;
        assert_eq!(s,StatusCode::OK,"{result}");
    }
    let(_,report)=a.call("GET","/v1/usage",Auth::Cookie(&cookie),None).await;
    assert_eq!(report["days"].as_array().unwrap().len(),1);
    assert_eq!(report["days"][0]["data"]["counters"]["apps_paused"],2);
    let(other_cookie,_)=a.web(&EvmWallet::new(62)).await;
    let(_,report)=a.call("GET","/v1/usage",Auth::Cookie(&other_cookie),None).await;
    assert_eq!(report["days"].as_array().unwrap().len(),0);
    let mut private=payload.clone();private["days"][0]["title"]=json!("private tab");
    let(s,_)=a.call("POST","/v1/usage/daily",Auth::Bearer(&token),Some(private)).await;
    assert_eq!(s,StatusCode::UNPROCESSABLE_ENTITY);
    let(s,_)=a.call("POST","/v1/usage/daily",Auth::Cookie(&cookie),Some(payload.clone())).await;
    assert_eq!(s,StatusCode::UNAUTHORIZED,"a website session is not a device token");
    a.call("PUT","/v1/account/prefs",Auth::Cookie(&cookie),Some(json!({"share_usage":false}))).await;
    let(s,_)=a.call("POST","/v1/usage/daily",Auth::Bearer(&token),Some(payload.clone())).await;
    assert_eq!(s,StatusCode::FORBIDDEN);
    a.call("PUT","/v1/account/prefs",Auth::Cookie(&cookie),Some(json!({"share_usage":true}))).await;
    a.store.revoke_device(&id,&device.id).await.unwrap();
    let(s,_)=a.call("POST","/v1/usage/daily",Auth::Bearer(&token),Some(payload)).await;
    assert_eq!(s,StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_existing_session_explicitly_approves_its_named_device() {
    let a=app();let(cookie,id)=a.web(&EvmWallet::new(63)).await;
    let(raw,code)=a.start_device().await;
    let body=json!({"user_code":code});
    let(s,_)=a.call("POST","/v1/device/approve",Auth::None,Some(body.clone())).await;
    assert_eq!(s,StatusCode::UNAUTHORIZED);
    let(s,_,_)=a.raw("POST","/v1/device/approve",Auth::Cookie(&cookie),"https://other.example",Some(body.clone())).await;
    assert_eq!(s,StatusCode::FORBIDDEN);
    let(s,result)=a.call("POST","/v1/device/approve",Auth::Cookie(&cookie),Some(body.clone())).await;
    assert_eq!(s,StatusCode::OK,"{result}");
    let(s,_)=a.call("POST","/v1/device/approve",Auth::Cookie(&cookie),Some(body)).await;
    assert_ne!(s,StatusCode::OK,"a code is approved once");
    let(s,token)=a.call("POST","/v1/device/token",Auth::None,Some(json!({"device_code":raw}))).await;
    assert_eq!(s,StatusCode::OK,"{token}");assert_eq!(token["account_id"],id);
    let(_,prefs)=a.call("GET","/v1/usage/config",Auth::Bearer(token["device_token"].as_str().unwrap()),None).await;
    assert_eq!(prefs["enabled"],false,"enrolling is not usage consent");
}
