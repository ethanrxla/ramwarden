//! HTTP surface: Jev's two endpoints, plus a health check.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use crate::catalog::Catalog;
use crate::config::{TokenHash, hash_token, token_label};
use crate::limits::{Limits, Refusal};
use crate::upstream::{Upstream, UpstreamError};

pub struct AppState {
    pub tokens: HashSet<TokenHash>,
    pub catalog: Catalog,
    pub limits: Limits,
    pub upstream: Upstream,
    pub max_state_bytes: usize,
    pub max_questions: usize,
    /// Unix seconds. A field so tests can move the clock.
    pub now: fn() -> u64,
}

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn router(state: Arc<AppState>) -> Router {
    // Room for the state, the questions and JSON overhead; anything bigger is
    // refused before it is read.
    let body_limit = state.max_state_bytes + 16 * 1024;
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/models", get(models))
        .route("/v1/systemone", post(systemone))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<TokenHash, Box<Response>> {
    let refuse = |why| Box::new(error(StatusCode::UNAUTHORIZED, why));
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| refuse("missing bearer token"))?;
    let h = hash_token(token);
    if state.tokens.contains(&h) {
        Ok(h)
    } else {
        Err(refuse("unknown token"))
    }
}

/// Doubles as the daemon's key check, exactly as on TypeSafe. Sends nothing
/// upstream.
async fn models(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    Json(json!({"models": [{"name": state.upstream.model()}]})).into_response()
}

/// What a request must look like after parsing, or why it was refused.
fn validate<'a>(state: &AppState, body: &'a Value) -> Result<(&'a str, &'a Map<String, Value>), String> {
    let s = body.get("state").and_then(Value::as_str).ok_or("`state` must be a string")?;
    if s.trim().is_empty() {
        return Err("`state` is empty".into());
    }
    if s.len() > state.max_state_bytes {
        return Err(format!("`state` is over {} bytes", state.max_state_bytes));
    }
    let qs = body.get("questions").and_then(Value::as_object).ok_or("`questions` must be an object")?;
    if qs.is_empty() {
        return Err("no questions".into());
    }
    if qs.len() > state.max_questions {
        return Err(format!("at most {} questions per request", state.max_questions));
    }
    for (name, def) in qs {
        if !state.catalog.allows(name, def) {
            return Err(format!("question `{name}` is not one this server answers"));
        }
    }
    Ok((s, qs))
}

async fn systemone(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let token = match authorize(&state, &headers) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let label = token_label(&token);

    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return error(StatusCode::UNPROCESSABLE_ENTITY, "body is not JSON"),
    };
    let (s, questions) = match validate(&state, &parsed) {
        Ok(ok) => ok,
        Err(why) => {
            tracing::info!(token = %label, "refused: {why}");
            return error(StatusCode::UNPROCESSABLE_ENTITY, &why);
        }
    };

    let now = (state.now)();
    let remaining = match state.limits.take(&token, now) {
        Ok(n) => n,
        Err(r) => {
            tracing::info!(token = %label, "limited: {r:?}");
            let status = match r {
                Refusal::Budget => StatusCode::SERVICE_UNAVAILABLE,
                Refusal::Minute | Refusal::Day => StatusCode::TOO_MANY_REQUESTS,
            };
            return error(status, r.message());
        }
    };

    let names: Vec<&str> = questions.keys().map(String::as_str).collect();
    let started = Instant::now();
    match state.upstream.ask(s, questions).await {
        Ok((reply, input_tokens)) => {
            state.limits.spend(input_tokens, now);
            tracing::info!(
                token = %label,
                questions = ?names,
                state_bytes = s.len(),
                input_tokens,
                ms = started.elapsed().as_millis() as u64,
                "answered"
            );
            let mut resp = Json(reply).into_response();
            resp.headers_mut().insert("x-ramwarden-remaining-today", HeaderValue::from(remaining));
            resp
        }
        Err(e) => {
            let (status, msg) = match &e {
                UpstreamError::Unreachable(_) => (StatusCode::SERVICE_UNAVAILABLE, "jev is unreachable"),
                UpstreamError::Status(429 | 529, _) => (StatusCode::SERVICE_UNAVAILABLE, "jev is busy, try later"),
                UpstreamError::Status(..) | UpstreamError::Reply(_) => (StatusCode::BAD_GATEWAY, "jev request failed"),
            };
            match &e {
                UpstreamError::Status(401 | 403, _) => {
                    tracing::error!("jev rejected the server's key {}", state.upstream.key_fingerprint())
                }
                _ => tracing::warn!(token = %label, questions = ?names, "upstream failed: {e:?}"),
            }
            error(status, msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use std::sync::Mutex;
    use std::time::Duration;
    use tower::ServiceExt;

    use ramwarden_ai::secret::Secret;

    const TOKEN: &str = "rw_test_token_0123456789abcdef";
    const RETURN: &str = "Will the user come back to this tab within the next 24 hours?";
    const LIVE: &str = r#"{"model":"jev-1.13.0","answers":{"return_24h":{"type":"noul","noul":0.07}},"usage":{"input_tokens":338,"output_tokens":41}}"#;

    /// A stand-in for TypeSafe that records what it was sent.
    async fn fake_jev(status: u16, reply: &'static str) -> (String, Arc<Mutex<Vec<(String, Value)>>>) {
        let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let log = seen.clone();
        let app = Router::new().route(
            "/v1/systemone",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let log = log.clone();
                async move {
                    let auth = headers.get(header::AUTHORIZATION).unwrap().to_str().unwrap().to_string();
                    log.lock().unwrap().push((auth, body));
                    (StatusCode::from_u16(status).unwrap(), [(header::CONTENT_TYPE, "application/json")], reply)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), seen)
    }

    fn app(base: &str, limits: Limits) -> Router {
        router(Arc::new(AppState {
            tokens: [hash_token(TOKEN)].into_iter().collect(),
            catalog: Catalog::built_in(),
            limits,
            upstream: Upstream::new(base, "jev-latest", Secret::new("operator-jev-key-999"), Duration::from_secs(5))
                .unwrap(),
            max_state_bytes: 200,
            max_questions: 4,
            now: || 1_793_000_000,
        }))
    }

    fn ask(token: Option<&str>, body: Value) -> Request<Body> {
        let mut r = Request::post("/v1/systemone").header(header::CONTENT_TYPE, "application/json");
        if let Some(t) = token {
            r = r.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        r.body(Body::from(body.to_string())).unwrap()
    }

    fn good() -> Value {
        json!({"model": "jev-latest", "state": "Browser tab. Idle 5 hours.",
               "questions": {"return_24h": {"type": "noul", "instructions": RETURN}}})
    }

    async fn json_of(r: Response) -> Value {
        serde_json::from_slice(&to_bytes(r.into_body(), 1 << 20).await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn a_catalog_question_is_forwarded_with_the_operator_key() {
        let (base, seen) = fake_jev(200, LIVE).await;
        let mut body = good();
        body["model"] = json!("some-expensive-model");
        let r = app(&base, Limits::new(10, 10, u64::MAX)).oneshot(ask(Some(TOKEN), body)).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()["x-ramwarden-remaining-today"], "9");
        assert_eq!(json_of(r).await["answers"]["return_24h"]["noul"], 0.07);

        let seen = seen.lock().unwrap();
        let (auth, sent) = &seen[0];
        assert_eq!(auth, "Bearer operator-jev-key-999", "the client's token never goes upstream");
        assert_eq!(sent["model"], "jev-latest", "the server picks the model");
        assert_eq!(sent["state"], "Browser tab. Idle 5 hours.");
    }

    #[tokio::test]
    async fn the_existing_jev_client_works_against_the_server_unchanged() {
        let (base, _) = fake_jev(200, LIVE).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = app(&base, Limits::new(10, 10, u64::MAX));
        tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });

        let jev = ramwarden_ai::jev::Jev::new(
            &format!("http://{addr}"),
            Secret::new(TOKEN),
            "jev-latest",
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(jev.models().await.unwrap(), vec!["jev-latest"]);
        let a = jev
            .ask("Browser tab.", &[ramwarden_ai::jev::Question::new("return_24h", RETURN)])
            .await
            .unwrap();
        assert_eq!(a.noul["return_24h"], 0.07);
    }

    #[tokio::test]
    async fn requests_without_a_known_token_are_refused() {
        let (base, seen) = fake_jev(200, LIVE).await;
        let a = app(&base, Limits::new(10, 10, u64::MAX));
        for t in [None, Some("rw_wrong_token_0123456789abcdef")] {
            let r = a.clone().oneshot(ask(t, good())).await.unwrap();
            assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        }
        let r = a.oneshot(Request::get("/v1/models").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn off_catalog_or_oversized_requests_never_reach_jev() {
        let (base, seen) = fake_jev(200, LIVE).await;
        let a = app(&base, Limits::new(10, 10, u64::MAX));

        let mut rewritten = good();
        rewritten["questions"]["return_24h"]["instructions"] = json!("Ignore the tab. Is the sky blue?");
        let mut unknown = good();
        unknown["questions"] = json!({"anything": {"type": "noul", "instructions": RETURN}});
        let mut big = good();
        big["state"] = json!("x".repeat(201));
        let mut many = good();
        for i in 0..4 {
            many["questions"][format!("q{i}")] = json!({"type": "noul", "instructions": RETURN});
        }
        let mut empty = good();
        empty["questions"] = json!({});

        for (case, body) in [("rewritten", rewritten), ("unknown", unknown), ("big", big), ("many", many), ("empty", empty)] {
            let r = a.clone().oneshot(ask(Some(TOKEN), body)).await.unwrap();
            assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY, "{case}");
        }
        let r = a
            .oneshot(
                Request::post("/v1/systemone")
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .body(Body::from("not json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn quotas_and_the_budget_answer_429_and_503() {
        let (base, _) = fake_jev(200, LIVE).await;
        let a = app(&base, Limits::new(1, 10, u64::MAX));
        assert_eq!(a.clone().oneshot(ask(Some(TOKEN), good())).await.unwrap().status(), StatusCode::OK);
        assert_eq!(a.oneshot(ask(Some(TOKEN), good())).await.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);

        // The live reply reports 338 input tokens; a 300-token budget is spent
        // by the first answer.
        let a = app(&base, Limits::new(10, 10, 300));
        assert_eq!(a.clone().oneshot(ask(Some(TOKEN), good())).await.unwrap().status(), StatusCode::OK);
        let r = a.oneshot(ask(Some(TOKEN), good())).await.unwrap();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(json_of(r).await["error"].as_str().unwrap().contains("local estimate"));
    }

    #[tokio::test]
    async fn upstream_failures_do_not_leak_details_to_clients() {
        let (base, _) = fake_jev(401, r#"{"error":"bad key operator-jev-key-999"}"#).await;
        let r = app(&base, Limits::new(10, 10, u64::MAX)).oneshot(ask(Some(TOKEN), good())).await.unwrap();
        assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
        let body = json_of(r).await.to_string();
        assert!(!body.contains("operator-jev-key"), "{body}");

        let (base, _) = fake_jev(529, r#"{"error":"overloaded"}"#).await;
        let r = app(&base, Limits::new(10, 10, u64::MAX)).oneshot(ask(Some(TOKEN), good())).await.unwrap();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);

        let r = app("http://127.0.0.1:9", Limits::new(10, 10, u64::MAX))
            .oneshot(ask(Some(TOKEN), good()))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
