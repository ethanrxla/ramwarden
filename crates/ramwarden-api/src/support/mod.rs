//! The site wardens as sales and support agents (product spec 2.6).
//!
//! The pet in the bottom-right corner of every page opens a chat. It answers
//! from the curated bank in [`kb`], routed by [`triage`], and when it cannot
//! help, or the visitor asks for a person, it opens a ticket the team answers
//! through the staff routes below. Conversations and tickets live in MongoDB
//! ([`ramwarden_cloud::support`]).
//!
//! Routes (the contract is in `docs/specs/website.md`, "Support chat"):
//!
//! | Route | Who | What |
//! |---|---|---|
//! | `POST /v1/support/history` | visitor | greeting, chips, and the stored thread if the secret is live |
//! | `POST /v1/support/chat` | visitor | one message (or a chip's topic) in, the warden's answer out |
//! | `POST /v1/support/ticket` | visitor | makes the conversation a ticket |
//! | `GET /v1/support/tickets` | signed in | the account's own tickets |
//! | `GET /v1/support/staff/tickets` | staff token | the queue, newest first |
//! | `GET /v1/support/staff/tickets/{ref}` | staff token | one ticket with its thread |
//! | `POST /v1/support/staff/tickets/{ref}/reply` | staff token | answers it |
//! | `POST /v1/support/staff/tickets/{ref}/status` | staff token | open, answered or closed |
//!
//! # What the warden may read
//!
//! A signed-in visitor's own plan, trial date and device count, to answer
//! "what plan am I on". Nothing else, and it changes nothing: no payment, no
//! plan change, no other account.
//!
//! # Abuse
//!
//! The chat is open to anyone, so it is limited three ways: per conversation
//! (a minute and a day), new conversations across the whole server, and the
//! same daily Jev budget the daemon's questions draw on. Over the budget the
//! warden still answers, from the keyword router.

// Handlers return `Response` errors, as in `accounts.rs`.
#![allow(clippy::result_large_err)]

pub mod kb;
pub mod triage;

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use ramwarden_cloud::model::{Account, Identity, Status};
use ramwarden_cloud::support::{
    Conversation, MAX_TEXT_CHARS, Speaker, SupportMessage, Ticket, TicketQuery, TicketStatus, normalise_reference,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::accounts::{self, Cloud, Reply, storage};
use crate::config::{TokenHash, hash_token};
use crate::limits::{Limits, Refusal};
use crate::routes::{AppState, error};

/// One conversation: messages per minute and per day.
pub const PER_MINUTE: u32 = 8;
pub const PER_DAY: u32 = 80;
/// New conversations across the server, per minute and per day.
pub const STARTS_PER_MINUTE: u32 = 60;
pub const STARTS_PER_DAY: u32 = 5000;

/// The support desk's own state, beside the rest of [`Cloud`].
pub struct SupportDesk {
    messages: Limits,
    starts: Limits,
    /// SHA-256 of `RAMWARDEN_SUPPORT_TOKEN`. `None`: staff routes answer 503.
    staff: Option<TokenHash>,
}

impl SupportDesk {
    pub fn new(staff: Option<TokenHash>) -> Self {
        SupportDesk {
            messages: Limits::new(PER_MINUTE, PER_DAY, u64::MAX),
            starts: Limits::new(STARTS_PER_MINUTE, STARTS_PER_DAY, u64::MAX),
            staff,
        }
    }

    fn staff(&self, headers: &HeaderMap) -> Result<(), Response> {
        let Some(want) = &self.staff else {
            return Err(error(StatusCode::SERVICE_UNAVAILABLE, "Staff support routes are off: RAMWARDEN_SUPPORT_TOKEN is not set."));
        };
        match accounts::bearer(headers).map(hash_token) {
            Some(got) if &got == want => Ok(()),
            _ => Err(error(StatusCode::UNAUTHORIZED, "This needs the support team's token.")),
        }
    }
}

pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/v1/support/history", post(history))
        .route("/v1/support/chat", post(chat))
        .route("/v1/support/ticket", post(open_ticket))
        .route("/v1/support/tickets", get(my_tickets))
        .route("/v1/support/staff/tickets", get(staff_list))
        .route("/v1/support/staff/tickets/{reference}", get(staff_get))
        .route("/v1/support/staff/tickets/{reference}/reply", post(staff_reply))
        .route("/v1/support/staff/tickets/{reference}/status", post(staff_status))
}

fn warden(raw: Option<&str>) -> &'static str {
    if raw == Some("threat") { "threat" } else { "ram" }
}

fn chips(ids: &[&str]) -> Value {
    ids.iter().map(|id| json!({"topic": id, "label": kb::chip(id)})).collect()
}

fn ticket_json(t: &Ticket) -> Value {
    json!({"reference": t.reference, "status": t.status.as_str(), "subject": t.subject, "opened_at": t.opened_at})
}

fn message_json(m: &SupportMessage) -> Value {
    let from = match m.from {
        Speaker::Visitor => "visitor",
        Speaker::Warden => "warden",
        Speaker::Staff => "staff",
    };
    json!({"from": from, "text": m.text, "at": m.at})
}

fn thread_json(c: &Conversation) -> Value {
    json!({
        "warden": c.warden,
        "messages": c.messages.iter().map(message_json).collect::<Vec<_>>(),
        "ticket": c.ticket.as_ref().map(ticket_json),
    })
}

/// The signed-in account, if there is one. Any failure (no cookie, expired,
/// wrong origin) is simply "signed out": the chat works either way.
async fn visitor(c: &Cloud, method: &Method, headers: &HeaderMap, now: u64) -> Option<Account> {
    accounts::session_token(headers)?;
    accounts::session(c, method, headers, now).await.ok()
}

/// "What plan am I on", from the account itself.
async fn account_answer(c: &Cloud, a: &Account, now: u64) -> Result<String, Response> {
    let day = |t: u64| crate::wallet::iso8601(t).chars().take(10).collect::<String>();
    let plan = match accounts::plan_name(a, now) {
        "pro" => "RamWarden Pro",
        "plus" => "RamWarden Plus",
        _ => "RamWarden Lite",
    };
    let state = match a.status(now) {
        None => "the free plan".to_string(),
        Some(Status::Trial) => format!("in your free trial until {}", day(a.trial_ends_at.unwrap_or(now))),
        Some(Status::Active) => format!("active, next charge around {}", day(a.paid_until.unwrap_or(now))),
        Some(Status::Cancelled) => format!("cancelled, and working until {}", day(a.paid_until.unwrap_or(now))),
        Some(Status::Grace) => "past due: the last renewal didn't go through, so check your payment method on the account page".into(),
        Some(Status::Lapsed) => "lapsed, so your computers run on the free algorithm until you renew".into(),
    };
    let used = c.store.devices(&a.id).await.map_err(storage)?.len();
    let limit = accounts::device_limit(a, now);
    Ok(format!("You're on {plan}: {state}. {used} of {limit} computers are linked. The account page has the details."))
}

#[derive(Deserialize)]
struct HistoryBody {
    #[serde(default)]
    conversation: Option<String>,
    #[serde(default)]
    warden: Option<String>,
}

/// `POST /v1/support/history`: what the chat shows when it opens.
async fn history(State(state): State<Arc<AppState>>, Json(b): Json<HistoryBody>) -> Reply {
    let c = accounts::cloud(&state)?;
    let now = (state.now)();
    let w = warden(b.warden.as_deref());
    let thread = match b.conversation.as_deref().filter(|s| s.starts_with("rwsc_")) {
        Some(raw) => c.store.conversation(raw, now).await.map_err(storage)?,
        None => None,
    };
    Ok(Json(json!({
        "greeting": kb::greeting(w),
        "chips": chips(kb::starters(w)),
        "live": thread.is_some(),
        "thread": thread.as_ref().map(thread_json),
    }))
    .into_response())
}

#[derive(Deserialize)]
struct ChatBody {
    #[serde(default)]
    conversation: Option<String>,
    #[serde(default)]
    warden: Option<String>,
    /// What the visitor typed.
    #[serde(default)]
    message: Option<String>,
    /// Or the chip they clicked: a topic id.
    #[serde(default)]
    topic: Option<String>,
}

fn limited(r: Refusal) -> Response {
    let msg = match r {
        Refusal::Minute => "Slow down a little, I'm reading as fast as I can. Try again in a minute.",
        _ => "That's a lot of questions for one day. Open a ticket and a person will pick it up.",
    };
    error(StatusCode::TOO_MANY_REQUESTS, msg)
}

/// `POST /v1/support/chat`.
async fn chat(State(state): State<Arc<AppState>>, method: Method, headers: HeaderMap, Json(b): Json<ChatBody>) -> Reply {
    let c = accounts::cloud(&state)?;
    let now = (state.now)();
    let w = warden(b.warden.as_deref());

    let chip = b.topic.as_deref().and_then(triage::for_chip);
    let text = match (&chip, b.message.as_deref().map(str::trim)) {
        (Some(t), _) => kb::chip(t.topic).to_string(),
        (None, Some(m)) if !m.is_empty() && m.chars().count() <= MAX_TEXT_CHARS => m.to_string(),
        (None, Some(m)) if !m.is_empty() => {
            return Err(error(StatusCode::UNPROCESSABLE_ENTITY, &format!("Keep it under {MAX_TEXT_CHARS} characters, please.")));
        }
        _ => return Err(error(StatusCode::UNPROCESSABLE_ENTITY, "Ask me something first.")),
    };

    let who = visitor(c, &method, &headers, now).await;
    let account_id = who.as_ref().map(|a| a.id.as_str());

    // An existing conversation, or a new one (counted against the server's
    // allowance for new ones).
    let existing = match b.conversation.as_deref().filter(|s| s.starts_with("rwsc_")) {
        Some(raw) => c.store.conversation(raw, now).await.map_err(storage)?.map(|_| raw.to_string()),
        None => None,
    };
    let (secret, fresh) = match existing {
        Some(raw) => (raw, false),
        None => {
            c.support.starts.take(&[0; 32], now).map_err(limited)?;
            let (raw, _) = c.store.start_conversation(w, account_id, now).await.map_err(storage)?;
            (raw, true)
        }
    };
    c.support.messages.take(&hash_token(&secret), now).map_err(limited)?;

    let t = match chip {
        Some(t) => t,
        None => {
            let upstream = state.limits.budget_left(now).then_some(&state.upstream);
            let t = triage::triage(upstream, w, &text).await;
            state.limits.spend(t.input_tokens, now);
            t
        }
    };
    let topic = kb::topic(t.topic);
    let answer = match (t.topic, &who) {
        ("my_account", Some(a)) => account_answer(c, a, now).await?,
        _ => topic.map_or(kb::unknown_answer(), |k| k.answer).to_string(),
    };
    let offer_ticket = t.wants_person || topic.is_none_or(|k| k.ticket);
    tracing::info!(warden = w, topic = t.topic, via = t.via, chars = text.len(), "support answer");

    let msgs = [SupportMessage::new(Speaker::Visitor, &text, now), SupportMessage::warden(&answer, t.topic, now)];
    let conv = c.store.append(&secret, &msgs, account_id, now).await.map_err(storage)?;
    let next = topic.map_or(&[][..], |k| k.next);
    Ok(Json(json!({
        "conversation": fresh.then_some(&secret),
        "reply": {
            "text": answer,
            "topic": t.topic,
            "chips": chips(next),
            "offer_ticket": offer_ticket && conv.as_ref().is_none_or(|c| c.ticket.is_none()),
        },
        "ticket": conv.as_ref().and_then(|c| c.ticket.as_ref()).map(ticket_json),
    }))
    .into_response())
}

#[derive(Deserialize)]
struct TicketBody {
    conversation: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    subject: Option<String>,
}

/// `POST /v1/support/ticket`.
async fn open_ticket(State(state): State<Arc<AppState>>, method: Method, headers: HeaderMap, Json(b): Json<TicketBody>) -> Reply {
    let c = accounts::cloud(&state)?;
    let now = (state.now)();
    let gone = || error(StatusCode::NOT_FOUND, "That chat has expired. Ask me again and I'll open a ticket from the new one.");
    let conv = c.store.conversation(&b.conversation, now).await.map_err(storage)?.ok_or_else(gone)?;
    let who = visitor(c, &method, &headers, now).await;

    let email = match b.email.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
        Some(e) if crate::signin::plausible_email(e) => Some(e.to_ascii_lowercase()),
        Some(_) => return Err(error(StatusCode::UNPROCESSABLE_ENTITY, "That email address doesn't look right.")),
        None => match who.as_ref().map(|a| &a.identity) {
            Some(Identity::Email { email }) => Some(email.clone()),
            _ => None,
        },
    };
    let subject = b
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| conv.messages.iter().rev().find(|m| m.from == Speaker::Visitor).map(|m| m.text.clone()))
        .unwrap_or_else(|| "Question from the website".into());

    let t = c.store.open_ticket(&b.conversation, &subject, email.as_deref(), now).await.map_err(storage)?.ok_or_else(gone)?;
    let reply = match &email {
        Some(e) => format!("Ticket {} is open. The team will write back to {e}, and the reply shows here too.", t.reference),
        None => format!("Ticket {} is open. Come back to this chat for the team's reply.", t.reference),
    };
    if conv.ticket.is_none() {
        let note = SupportMessage::warden(&reply, "ticket", now);
        c.store.append(&b.conversation, &[note], who.as_ref().map(|a| a.id.as_str()), now).await.map_err(storage)?;
        tracing::info!(reference = %t.reference, warden = %conv.warden, "support ticket opened");
    }
    Ok(Json(json!({"ticket": ticket_json(&t), "reply": {"text": reply}})).into_response())
}

/// `GET /v1/support/tickets`: the signed-in account's own.
async fn my_tickets(State(state): State<Arc<AppState>>, method: Method, headers: HeaderMap) -> Reply {
    let c = accounts::cloud(&state)?;
    let a = accounts::session(c, &method, &headers, (state.now)()).await?;
    let list = c.store.account_tickets(&a.id).await.map_err(storage)?;
    Ok(Json(json!({"tickets": list.iter().filter_map(|c| c.ticket.as_ref()).map(ticket_json).collect::<Vec<_>>()})).into_response())
}

// ------------------------------------------------------------------ staff

fn staff_json(c: &Conversation) -> Value {
    let mut v = thread_json(c);
    if let Some(t) = &c.ticket {
        v["ticket"]["email"] = json!(t.email);
    }
    v["account_id"] = json!(c.account_id);
    v["updated_at"] = json!(c.updated_at);
    // Which bank topics the warden answered from, so a wrong answer is easy
    // to trace back to the entry that needs fixing.
    v["topics"] = c.messages.iter().filter_map(|m| m.topic.clone()).collect();
    v
}

fn reference(raw: &str) -> Result<String, Response> {
    normalise_reference(raw).ok_or_else(|| error(StatusCode::NOT_FOUND, "No ticket has that reference."))
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

async fn staff_list(State(state): State<Arc<AppState>>, headers: HeaderMap, Query(q): Query<ListQuery>) -> Reply {
    let c = accounts::cloud(&state)?;
    c.support.staff(&headers)?;
    let status = match q.status.as_deref() {
        None | Some("all") => None,
        Some(s) => Some(TicketStatus::parse(s).ok_or_else(|| error(StatusCode::UNPROCESSABLE_ENTITY, "status is open, answered, closed or all"))?),
    };
    let list = c.store.tickets(TicketQuery { status, limit: q.limit.unwrap_or(50).min(200) }).await.map_err(storage)?;
    Ok(Json(json!({"tickets": list.iter().map(staff_json).collect::<Vec<_>>()})).into_response())
}

async fn staff_get(State(state): State<Arc<AppState>>, headers: HeaderMap, Path(r): Path<String>) -> Reply {
    let c = accounts::cloud(&state)?;
    c.support.staff(&headers)?;
    let conv = c.store.ticket(&reference(&r)?).await.map_err(storage)?;
    let conv = conv.ok_or_else(|| error(StatusCode::NOT_FOUND, "No ticket has that reference."))?;
    Ok(Json(staff_json(&conv)).into_response())
}

#[derive(Deserialize)]
struct ReplyBody {
    text: String,
}

async fn staff_reply(State(state): State<Arc<AppState>>, headers: HeaderMap, Path(r): Path<String>, Json(b): Json<ReplyBody>) -> Reply {
    let c = accounts::cloud(&state)?;
    c.support.staff(&headers)?;
    if b.text.trim().is_empty() {
        return Err(error(StatusCode::UNPROCESSABLE_ENTITY, "The reply is empty."));
    }
    let conv = c.store.staff_reply(&reference(&r)?, &b.text, (state.now)()).await.map_err(storage)?;
    let conv = conv.ok_or_else(|| error(StatusCode::NOT_FOUND, "No ticket has that reference."))?;
    Ok(Json(staff_json(&conv)).into_response())
}

#[derive(Deserialize)]
struct StatusBody {
    status: String,
}

async fn staff_status(State(state): State<Arc<AppState>>, headers: HeaderMap, Path(r): Path<String>, Json(b): Json<StatusBody>) -> Reply {
    let c = accounts::cloud(&state)?;
    c.support.staff(&headers)?;
    let s = TicketStatus::parse(&b.status).ok_or_else(|| error(StatusCode::UNPROCESSABLE_ENTITY, "status is open, answered or closed"))?;
    if !c.store.set_ticket_status(&reference(&r)?, s, (state.now)()).await.map_err(storage)? {
        return Err(error(StatusCode::NOT_FOUND, "No ticket has that reference."));
    }
    Ok(Json(json!({"status": s.as_str()})).into_response())
}
