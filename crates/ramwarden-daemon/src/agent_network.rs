//! Explicit owner-driven guest pairing. Peer content grants no execution authority.
use axum::{
    Extension, Json, Router,
    extract::Path,
    http::StatusCode,
    routing::{get, post},
};
use ramwarden_agent_network::{Client, Identity};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

pub struct Service {
    folder: PathBuf,
    endpoint: String,
}
impl Service {
    pub fn new(folder: PathBuf) -> Self {
        // Development override is still checked by Client's fixed endpoint allowlist.
        Self {
            folder,
            endpoint: std::env::var("RAMWARDEN_AGENT_NETWORK_URL")
                .unwrap_or_else(|_| "https://scholarcal.com/api/agent-network/v1".into()),
        }
    }
    async fn client(&self) -> Result<Client, Failure> {
        let folder = self.folder.clone();
        let identity = tokio::task::spawn_blocking(move || Identity::load_or_create(&folder))
            .await
            .map_err(|_| failure("Cannot prepare agent identity"))?
            .map_err(failure)?;
        Client::new(&self.endpoint, identity).map_err(failure)
    }
}
type Failure = (StatusCode, Json<Value>);
fn failure(message: impl Into<String>) -> Failure {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"error":message.into()})),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Register {
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invitation {
    invitation: String,
}
async fn status(Extension(service): Extension<Arc<Service>>) -> Json<Value> {
    Json(
        json!({"identitySaved":service.folder.join("identity.json").is_file(),"connected":false,"detail":"Connect to check hosted agent pairing. ScholarCal sign-in is optional."}),
    )
}
async fn connect(
    Extension(service): Extension<Arc<Service>>,
    Json(body): Json<Register>,
) -> Result<Json<Value>, Failure> {
    if body.name.is_empty()
        || body.name.chars().count() > 64
        || body.name.chars().any(char::is_control)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Choose a name of 1–64 characters"})),
        ));
    }
    let client = service.client().await?;
    let did = client.register(&body.name).await.map_err(failure)?;
    Ok(Json(
        json!({"did":did,"pairs":client.pairs().await.map_err(failure)?}),
    ))
}
async fn invite(Extension(service): Extension<Arc<Service>>) -> Result<Json<Value>, Failure> {
    Ok(Json(
        service
            .client()
            .await?
            .rpc("pair/invite", json!({}))
            .await
            .map_err(failure)?,
    ))
}
async fn accept(
    Extension(service): Extension<Arc<Service>>,
    Json(body): Json<Invitation>,
) -> Result<Json<Value>, Failure> {
    if body.invitation.len() > 180 || !body.invitation.starts_with("agn1.did:agn:") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Invalid pairing invitation"})),
        ));
    }
    Ok(Json(
        service
            .client()
            .await?
            .rpc("pair/accept", json!({"invitation":body.invitation}))
            .await
            .map_err(failure)?,
    ))
}
async fn pairs(Extension(service): Extension<Arc<Service>>) -> Result<Json<Value>, Failure> {
    Ok(Json(
        json!({"pairs":service.client().await?.pairs().await.map_err(failure)?}),
    ))
}
async fn confirm(
    Extension(service): Extension<Arc<Service>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let client = service.client().await?;
    // Verify the peer's signed registration before approving it.
    let known = client.pairs().await.map_err(failure)?;
    if !known
        .iter()
        .any(|pair| pair.id == id && pair.needs_approval)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"No matching peer awaits your approval"})),
        ));
    }
    Ok(Json(
        client
            .rpc("pair/confirm", json!({"id":id}))
            .await
            .map_err(failure)?,
    ))
}
async fn revoke(
    Extension(service): Extension<Arc<Service>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    Ok(Json(
        service
            .client()
            .await?
            .rpc("pair/revoke", json!({"id":id}))
            .await
            .map_err(failure)?,
    ))
}
pub fn routes(folder: PathBuf) -> Router<crate::hub::AppState> {
    Router::new()
        .route("/agent-network/status", get(status))
        .route("/agent-network/connect", post(connect))
        .route("/agent-network/invites", post(invite))
        .route("/agent-network/accept", post(accept))
        .route("/agent-network/pairs", get(pairs))
        .route("/agent-network/pairs/{id}/confirm", post(confirm))
        .route("/agent-network/pairs/{id}/revoke", post(revoke))
        .layer(Extension(Arc::new(Service::new(folder))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    #[tokio::test]
    async fn offline_status_does_not_create_identity_or_connect() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("identity");
        let result = status(Extension(Arc::new(Service::new(folder.clone())))).await;
        assert_eq!(result.0["identitySaved"], false);
        assert!(!folder.exists());
    }
    #[tokio::test]
    async fn invalid_registration_fails_before_identity_creation() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("identity");
        let result = connect(
            Extension(Arc::new(Service::new(folder.clone()))),
            Json(Register { name: "\n".into() }),
        )
        .await;
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert!(!folder.exists());
    }
    #[tokio::test]
    async fn browser_page_cannot_request_pairing() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("identity");
        let policy = crate::guard::Policy::for_bind("127.0.0.1", 7823);
        let app = Router::new()
            .route("/connect", post(connect))
            .layer(Extension(Arc::new(Service::new(folder.clone()))))
            .layer(axum::middleware::from_fn_with_state(
                policy,
                crate::guard::enforce,
            ));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connect")
                    .header("origin", "https://example.com")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"Unwanted"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!folder.exists());
        assert!(
            !to_bytes(response.into_body(), 4096)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
