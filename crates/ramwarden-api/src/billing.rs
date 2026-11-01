//! Checkout and subscription routes (product spec 1.2, 3.3, 3.4).
//!
//! Both ways to pay start the same thing: a 14-day trial of the chosen tier
//! and interval that then renews until cancelled. One trial per account; an
//! account that had one is charged at once.
//!
//! - **Base**: the server builds the wallet's transactions
//!   ([`crate::checkout`]); the chain watcher records what the contract did.
//! - **Card**: Stripe Checkout in subscription mode ([`crate::stripe`]); the
//!   signed webhook records what Stripe did.
//!
//! Neither route changes an account by itself. Only the watcher and the
//! webhook do, from what actually happened, so `status` never says "active"
//! for a checkout that was abandoned.

#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use ramwarden_cloud::model::{
    Account, BillingMethod, BillingUpdate, Identity, Payment, PaymentStatus, Status,
};
use ramwarden_cloud::store::{new_id, random_bytes};
use serde::Deserialize;
use serde_json::json;

use crate::accounts::{Cloud, Reply, cloud, session, storage};
use crate::checkout::{BaseChain, PlanDef, usdc_minor};
use crate::routes::{AppState, error};
use crate::stripe::{self, CheckoutRequest, Stripe};
use crate::wallet::{self, Chain, iso8601};

/// The payment rails this server has configured.
#[derive(Default)]
pub struct Billing {
    pub base: Option<Arc<BaseChain>>,
    pub stripe: Option<Stripe>,
}

pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/v1/checkout/config", get(config))
        .route("/v1/checkout/base", post(checkout_base))
        .route("/v1/checkout/stripe", post(checkout_stripe))
        .route("/v1/checkout/solana", post(solana))
        .route("/v1/checkout/payments/{id}", get(payment))
        .route("/v1/checkout/cancel", post(cancel))
        .route("/v1/stripe/webhook", post(webhook))
}

async fn config(State(state): State<Arc<AppState>>) -> Reply {
    let c = cloud(&state)?;
    let b = &c.billing;
    let json = c.plans.config_json(b.base.as_deref().map(|b| &b.cfg), b.stripe.as_ref().map(Stripe::test_mode));
    Ok(Json(json).into_response())
}

#[derive(Deserialize)]
struct CheckoutReq {
    plan: u32,
    #[serde(default)]
    payer: Option<String>,
}

/// The signed-in account and the plan it picked, unless it already has a
/// live subscription. Changing tier or interval is cancel, then subscribe:
/// in-place plan changes come later (see the spec's limitations).
async fn start<'a>(
    c: &'a Cloud,
    method: &Method,
    headers: &HeaderMap,
    plan: u32,
    now: u64,
) -> Result<(Account, &'a PlanDef), Response> {
    let a = session(c, method, headers, now).await?;
    let plan = c.plans.plan(plan).ok_or_else(|| error(StatusCode::NOT_FOUND, "No such plan."))?;
    if matches!(a.status(now), Some(Status::Trial | Status::Active | Status::Grace)) {
        return Err(error(
            StatusCode::CONFLICT,
            "This account already has a subscription. To switch plans, cancel it first; it runs to the end of what you paid for.",
        ));
    }
    Ok((a, plan))
}

fn pending(id: String, a: &Account, plan: &PlanDef, method: BillingMethod, payer: Option<String>, now: u64) -> Payment {
    Payment {
        id,
        account_id: a.id.clone(),
        plan: plan.id,
        method,
        status: PaymentStatus::Pending,
        reason: None,
        payer,
        created_at: now,
        external_id: None,
    }
}

async fn checkout_base(
    State(state): State<Arc<AppState>>,
    method: Method,
    headers: HeaderMap,
    Json(b): Json<CheckoutReq>,
) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let base = c.billing.base.as_ref().ok_or_else(|| error(StatusCode::SERVICE_UNAVAILABLE, "Paying on Base opens soon."))?;
    let (a, plan) = start(c, &method, &headers, b.plan, now).await?;
    let payer = match &b.payer {
        Some(p) => Some(wallet::normalise(Chain::Evm, p).map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, &e))?),
        None => None,
    };
    let cap = usdc_minor(plan.approval_usd());
    let needs_approve = match &payer {
        Some(p) => base.needs_approve(p, cap).await,
        None => true,
    };
    let payment_ref = random_bytes::<32>();
    let id = hex::encode(payment_ref);
    c.store.create_payment(pending(id.clone(), &a, plan, BillingMethod::Base, payer, now)).await.map_err(storage)?;
    Ok(Json(json!({
        "payment_id": id,
        "chain_id": base.cfg.chain_id,
        "trial": !a.trial_used,
        "approval_usd": plan.approval_usd(),
        "transactions": base.cfg.transactions(&a.id, plan, &payment_ref, needs_approve),
    }))
    .into_response())
}

async fn checkout_stripe(
    State(state): State<Arc<AppState>>,
    method: Method,
    headers: HeaderMap,
    Json(b): Json<CheckoutReq>,
) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let s = c
        .billing
        .stripe
        .as_ref()
        .ok_or_else(|| error(StatusCode::SERVICE_UNAVAILABLE, "Card payments open soon. Pay with USDC on Base."))?;
    let (a, plan) = start(c, &method, &headers, b.plan, now).await?;
    let id = new_id();
    let email = match &a.identity {
        Identity::Email { email } => Some(email.as_str()),
        Identity::Wallet { .. } => None,
    };
    let done = format!("{}/account.html?checkout=done&payment={id}", c.site.origin);
    let back = format!("{}/account.html?checkout=cancelled", c.site.origin);
    let req = CheckoutRequest {
        account_id: &a.id,
        payment_id: &id,
        plan,
        email,
        trial: !a.trial_used,
        success_url: &done,
        cancel_url: &back,
    };
    let url = s.checkout(&req).await.map_err(|e| {
        tracing::error!("{e}");
        error(StatusCode::BAD_GATEWAY, "Card checkout could not start. Try again in a minute.")
    })?;
    c.store.create_payment(pending(id.clone(), &a, plan, BillingMethod::Card, None, now)).await.map_err(storage)?;
    Ok(Json(json!({"payment_id": id, "url": url, "trial": !a.trial_used})).into_response())
}

async fn solana(State(state): State<Arc<AppState>>) -> Reply {
    cloud(&state)?;
    Err(error(StatusCode::SERVICE_UNAVAILABLE, "Paying on Solana opens soon. Pay with USDC on Base, or by card."))
}

/// The page polls this after the wallet sends, or after Stripe redirects
/// back, until it is confirmed.
async fn payment(State(state): State<Arc<AppState>>, method: Method, headers: HeaderMap, Path(id): Path<String>) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let a = session(c, &method, &headers, now).await?;
    let p = c
        .store
        .payment(&id)
        .await
        .map_err(storage)?
        .filter(|p| p.account_id == a.id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "No such payment on this account."))?;
    let until = a.served_until();
    Ok(Json(json!({
        "id": p.id,
        "plan": p.plan,
        "method": p.method,
        "status": p.status,
        "reason": p.reason,
        "account_status": a.status(now).map(|s| s.as_str()),
        "tier": a.tier.map(|t| t.as_str()),
        "trial_ends_at": a.trial_ends_at.map(iso8601),
        "entitlement_until": (until > now).then(|| iso8601(until)),
    }))
    .into_response())
}

/// Stops renewal. On Base only the paying wallet can, so this returns the
/// transaction for it to send; a card subscription is cancelled here.
/// Either way service runs to `paid_until`.
async fn cancel(State(state): State<Arc<AppState>>, method: Method, headers: HeaderMap) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let a = session(c, &method, &headers, now).await?;
    let nothing = || error(StatusCode::CONFLICT, "There is no subscription to cancel.");
    let billing = a.billing.as_ref().filter(|_| !a.cancelled && a.tier.is_some()).ok_or_else(nothing)?;
    match billing.method {
        BillingMethod::Base => {
            let base = c.billing.base.as_ref().ok_or_else(|| error(StatusCode::SERVICE_UNAVAILABLE, "Base is not configured on this server."))?;
            Ok(Json(json!({
                "chain_id": base.cfg.chain_id,
                "payer": billing.reference,
                "transactions": [base.cfg.cancel_transaction(&a.id)],
            }))
            .into_response())
        }
        BillingMethod::Card => {
            let s = c.billing.stripe.as_ref().ok_or_else(|| error(StatusCode::SERVICE_UNAVAILABLE, "Card payments are not configured on this server."))?;
            s.cancel_at_period_end(&billing.reference).await.map_err(|e| {
                tracing::error!("{e}");
                error(StatusCode::BAD_GATEWAY, "The cancellation did not go through. Try again in a minute.")
            })?;
            let u = BillingUpdate { cancelled: Some(true), ..Default::default() };
            let a = c.store.update_billing(&a.id, &u).await.map_err(storage)?.ok_or_else(nothing)?;
            Ok(Json(json!({"cancelled": true, "service_until": a.paid_until.map(iso8601)})).into_response())
        }
        BillingMethod::Solana => Err(nothing()),
    }
}

/// Stripe's webhook. The signature is checked on the raw body before it is
/// parsed; anything unsigned, stale or for an unknown account is dropped.
async fn webhook(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let Ok(c) = cloud(&state) else { return StatusCode::SERVICE_UNAVAILABLE.into_response() };
    let Some(s) = &c.billing.stripe else { return StatusCode::NOT_FOUND.into_response() };
    let now = (state.now)();
    let sig = headers.get("stripe-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !s.verify(sig, &body, now) {
        tracing::warn!("stripe webhook with a bad signature");
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(event) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some((account_id, update)) = stripe::update_from_event(&event, &c.plans) else {
        return StatusCode::OK.into_response();
    };
    match c.store.update_billing(&account_id, &update).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            tracing::warn!(account = %account_id, "stripe event for an unknown account");
            return StatusCode::OK.into_response();
        }
        // Stripe retries a non-2xx for three days.
        Err(e) => return storage(e),
    }
    if let Some((pid, sub)) = stripe::confirmed_payment(&event)
        && let Ok(Some(p)) = c.store.payment(pid).await
        && p.account_id == account_id
        && let Err(e) = c.store.confirm_payment(pid, sub).await
    {
        return storage(e);
    }
    tracing::info!(account = %account_id, kind = %event["type"].as_str().unwrap_or(""), "stripe event recorded");
    StatusCode::OK.into_response()
}
