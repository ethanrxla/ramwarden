//! The site wardens' conversations and the support tickets they open.
//!
//! A visitor talks to a warden pet on the website (product spec 2.6). Every
//! conversation is stored, because a support agent that forgets what it was
//! told cannot hand a problem on. It is stored under the rules the privacy
//! page states:
//!
//! - **Kept 30 days**, then deleted by a TTL index on `expire_at`. A
//!   conversation that became a ticket is kept a year from its last message,
//!   so a reply months later still has its history.
//! - **Owned by a bearer secret**, not by an IP or a cookie the API sets. The
//!   page keeps the raw secret in `localStorage`; the store keeps its SHA-256
//!   as `_id`, exactly as sessions are kept. Whoever holds the secret can read
//!   the thread and nobody else can, signed in or not.
//! - **Linked to an account only when the visitor was signed in**, so a
//!   signed-in user can see their tickets from any browser.
//! - **Bounded**: [`MAX_MESSAGES`] per conversation (the oldest drop off) and
//!   [`MAX_TEXT_CHARS`] per message, enforced here so no route can forget.
//!
//! Staff answer tickets through the API's support routes; their reply shows
//! in the visitor's chat the next time it opens.

use async_trait::async_trait;
use mongodb::bson::{self, Document, doc};
use mongodb::options::{IndexOptions, ReturnDocument};
use mongodb::{Collection, IndexModel};
use serde::{Deserialize, Serialize};

use crate::model::Id;
use crate::mongo::MongoStore;
use crate::store::{MemoryStore, Result, hash, random_bytes, random_token};

pub const SUPPORT_CONVERSATIONS: &str = "support_conversations";

/// How long an ordinary conversation is kept (spec 2.6, privacy page).
pub const CHAT_RETENTION_SECONDS: u64 = 30 * 86_400;
/// How long a ticket is kept after its last message.
pub const TICKET_RETENTION_SECONDS: u64 = 365 * 86_400;
/// Messages kept per conversation. A support chat longer than this is a
/// ticket, and the opening of it is what staff need least.
pub const MAX_MESSAGES: usize = 60;
/// Longest message stored, in characters.
pub const MAX_TEXT_CHARS: usize = 2000;

/// The ticket reference alphabet: the site's consonants plus digits that
/// cannot be misread (no 0, 1). Six of them is 1.1e9 references.
const REF_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ23456789";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    Visitor,
    /// The pet's own answer.
    Warden,
    /// A person on the RamWarden team.
    Staff,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportMessage {
    pub from: Speaker,
    pub text: String,
    pub at: u64,
    /// For a warden answer, the topic it answered from. Kept so staff can see
    /// where the answer bank was wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

impl SupportMessage {
    pub fn new(from: Speaker, text: &str, at: u64) -> Self {
        SupportMessage { from, text: clip(text), at, topic: None }
    }

    pub fn warden(text: &str, topic: &str, at: u64) -> Self {
        SupportMessage { topic: Some(topic.to_string()), ..Self::new(Speaker::Warden, text, at) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketStatus {
    /// Waiting on the team.
    Open,
    /// The team replied; waiting on the visitor.
    Answered,
    Closed,
}

impl TicketStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TicketStatus::Open => "open",
            TicketStatus::Answered => "answered",
            TicketStatus::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(TicketStatus::Open),
            "answered" => Some(TicketStatus::Answered),
            "closed" => Some(TicketStatus::Closed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    /// `RW-XXXXXX`, what the visitor quotes and staff look up.
    pub reference: String,
    pub status: TicketStatus,
    pub subject: String,
    /// Where to write back, when the visitor gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub opened_at: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    /// SHA-256 of the visitor's secret.
    #[serde(rename = "_id")]
    pub id: Id,
    /// `ram` or `threat`: which pet was asked.
    pub warden: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<Id>,
    pub messages: Vec<SupportMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<Ticket>,
    pub created_at: u64,
    pub updated_at: u64,
    /// Unix seconds. Mongo also stores it as the BSON date `expire_at`.
    pub expires_at: u64,
}

impl Conversation {
    fn retention(&self) -> u64 {
        if self.ticket.is_some() { TICKET_RETENTION_SECONDS } else { CHAT_RETENTION_SECONDS }
    }
}

fn clip(text: &str) -> String {
    text.trim().chars().take(MAX_TEXT_CHARS).collect()
}

pub fn ticket_reference() -> String {
    let b = random_bytes::<6>();
    let s: String = b.iter().map(|x| REF_ALPHABET[*x as usize % REF_ALPHABET.len()] as char).collect();
    format!("RW-{s}")
}

/// `RW-XXXXXX` from whatever was typed: case and spaces forgiven.
pub fn normalise_reference(raw: &str) -> Option<String> {
    let s: String = raw.chars().filter(|c| !c.is_whitespace() && *c != '-').collect::<String>().to_ascii_uppercase();
    let s = s.strip_prefix("RW").unwrap_or(&s);
    (s.len() == 6 && s.bytes().all(|b| REF_ALPHABET.contains(&b))).then(|| format!("RW-{s}"))
}

/// Which tickets a staff listing wants.
#[derive(Clone, Copy, Debug, Default)]
pub struct TicketQuery {
    pub status: Option<TicketStatus>,
    pub limit: u32,
}

#[async_trait]
pub trait SupportStore: Send + Sync {
    /// A new conversation. Returns the raw secret, for the visitor only.
    async fn start_conversation(&self, warden: &str, account_id: Option<&str>, now: u64) -> Result<(String, Conversation)>;
    /// The live conversation this secret owns.
    async fn conversation(&self, raw_secret: &str, now: u64) -> Result<Option<Conversation>>;
    /// Appends messages (keeping the newest [`MAX_MESSAGES`]) and pushes the
    /// expiry forward. A visitor message on an answered ticket reopens it.
    /// `account_id` links a conversation that started signed out.
    async fn append(&self, raw_secret: &str, msgs: &[SupportMessage], account_id: Option<&str>, now: u64) -> Result<Option<Conversation>>;
    /// Makes the conversation a ticket, once: a second call returns the
    /// ticket the first one opened.
    async fn open_ticket(&self, raw_secret: &str, subject: &str, email: Option<&str>, now: u64) -> Result<Option<Ticket>>;

    // --- staff
    async fn ticket(&self, reference: &str) -> Result<Option<Conversation>>;
    /// Newest activity first.
    async fn tickets(&self, q: TicketQuery) -> Result<Vec<Conversation>>;
    /// Adds a staff message and marks the ticket answered.
    async fn staff_reply(&self, reference: &str, text: &str, now: u64) -> Result<Option<Conversation>>;
    async fn set_ticket_status(&self, reference: &str, status: TicketStatus, now: u64) -> Result<bool>;

    // --- the signed-in visitor
    async fn account_tickets(&self, account_id: &str) -> Result<Vec<Conversation>>;
}

// ------------------------------------------------------------------ memory

/// In-memory state, kept beside [`MemoryStore`]'s own.
pub(crate) type MemorySupport = std::sync::Mutex<std::collections::HashMap<Id, Conversation>>;

fn live(c: &Conversation, now: u64) -> bool {
    c.expires_at > now
}

fn push(c: &mut Conversation, msgs: &[SupportMessage], now: u64) {
    c.messages.extend(msgs.iter().cloned().map(|mut m| {
        m.text = clip(&m.text);
        m
    }));
    let over = c.messages.len().saturating_sub(MAX_MESSAGES);
    c.messages.drain(..over);
    c.updated_at = now;
    if let Some(t) = &mut c.ticket
        && t.status == TicketStatus::Answered
        && msgs.iter().any(|m| m.from == Speaker::Visitor)
    {
        t.status = TicketStatus::Open;
    }
    c.expires_at = now + c.retention();
}

fn newest_first(mut v: Vec<Conversation>, limit: u32) -> Vec<Conversation> {
    v.sort_by_key(|c| std::cmp::Reverse(c.updated_at));
    v.truncate(limit.max(1) as usize);
    v
}

#[async_trait]
impl SupportStore for MemoryStore {
    async fn start_conversation(&self, warden: &str, account_id: Option<&str>, now: u64) -> Result<(String, Conversation)> {
        let raw = random_token("rwsc_");
        let c = Conversation {
            id: hash(&raw),
            warden: warden.to_string(),
            account_id: account_id.map(str::to_string),
            messages: vec![],
            ticket: None,
            created_at: now,
            updated_at: now,
            expires_at: now + CHAT_RETENTION_SECONDS,
        };
        self.support().insert(c.id.clone(), c.clone());
        Ok((raw, c))
    }

    async fn conversation(&self, raw_secret: &str, now: u64) -> Result<Option<Conversation>> {
        Ok(self.support().get(&hash(raw_secret)).filter(|c| live(c, now)).cloned())
    }

    async fn append(&self, raw_secret: &str, msgs: &[SupportMessage], account_id: Option<&str>, now: u64) -> Result<Option<Conversation>> {
        let mut g = self.support();
        let Some(c) = g.get_mut(&hash(raw_secret)).filter(|c| live(c, now)) else { return Ok(None) };
        push(c, msgs, now);
        if c.account_id.is_none() {
            c.account_id = account_id.map(str::to_string);
        }
        Ok(Some(c.clone()))
    }

    async fn open_ticket(&self, raw_secret: &str, subject: &str, email: Option<&str>, now: u64) -> Result<Option<Ticket>> {
        let mut g = self.support();
        let Some(c) = g.get_mut(&hash(raw_secret)).filter(|c| live(c, now)) else { return Ok(None) };
        if let Some(t) = &c.ticket {
            return Ok(Some(t.clone()));
        }
        let t = Ticket {
            reference: ticket_reference(),
            status: TicketStatus::Open,
            subject: clip(subject).chars().take(140).collect(),
            email: email.map(str::to_string),
            opened_at: now,
        };
        c.ticket = Some(t.clone());
        c.updated_at = now;
        c.expires_at = now + TICKET_RETENTION_SECONDS;
        Ok(Some(t))
    }

    async fn ticket(&self, reference: &str) -> Result<Option<Conversation>> {
        Ok(self.support().values().find(|c| c.ticket.as_ref().is_some_and(|t| t.reference == reference)).cloned())
    }

    async fn tickets(&self, q: TicketQuery) -> Result<Vec<Conversation>> {
        let v = self
            .support()
            .values()
            .filter(|c| c.ticket.as_ref().is_some_and(|t| q.status.is_none_or(|s| t.status == s)))
            .cloned()
            .collect();
        Ok(newest_first(v, q.limit))
    }

    async fn staff_reply(&self, reference: &str, text: &str, now: u64) -> Result<Option<Conversation>> {
        let mut g = self.support();
        let Some(c) = g.values_mut().find(|c| c.ticket.as_ref().is_some_and(|t| t.reference == reference)) else {
            return Ok(None);
        };
        push(c, &[SupportMessage::new(Speaker::Staff, text, now)], now);
        if let Some(t) = &mut c.ticket {
            t.status = TicketStatus::Answered;
        }
        Ok(Some(c.clone()))
    }

    async fn set_ticket_status(&self, reference: &str, status: TicketStatus, now: u64) -> Result<bool> {
        let mut g = self.support();
        let Some(c) = g.values_mut().find(|c| c.ticket.as_ref().is_some_and(|t| t.reference == reference)) else {
            return Ok(false);
        };
        if let Some(t) = &mut c.ticket {
            t.status = status;
        }
        c.updated_at = now;
        Ok(true)
    }

    async fn account_tickets(&self, account_id: &str) -> Result<Vec<Conversation>> {
        let v = self
            .support()
            .values()
            .filter(|c| c.ticket.is_some() && c.account_id.as_deref() == Some(account_id))
            .cloned()
            .collect();
        Ok(newest_first(v, 50))
    }
}

// ------------------------------------------------------------------- mongo

fn date(unix: u64) -> bson::DateTime {
    bson::DateTime::from_millis((unix as i64).saturating_mul(1000))
}

fn to_doc<T: Serialize>(v: &T) -> Document {
    bson::to_document(v).expect("support types serialize to BSON")
}

impl MongoStore {
    fn conversations(&self) -> Collection<Conversation> {
        self.database().collection(SUPPORT_CONVERSATIONS)
    }

    pub(crate) async fn ensure_support_indexes(&self) -> Result<()> {
        let opts = |name: &str| IndexOptions::builder().name(name.to_string());
        let models = [
            IndexModel::builder()
                .keys(doc! { "expire_at": 1 })
                .options(opts("expire").expire_after(std::time::Duration::ZERO).build())
                .build(),
            // Only tickets have a reference; the partial filter keeps the
            // many plain chats out of a unique index they would all collide in.
            IndexModel::builder()
                .keys(doc! { "ticket.reference": 1 })
                .options(
                    opts("ticket_reference_unique")
                        .unique(true)
                        .partial_filter_expression(doc! { "ticket.reference": { "$exists": true } })
                        .build(),
                )
                .build(),
            IndexModel::builder()
                .keys(doc! { "ticket.status": 1, "updated_at": -1 })
                .options(
                    opts("tickets_by_status")
                        .partial_filter_expression(doc! { "ticket.reference": { "$exists": true } })
                        .build(),
                )
                .build(),
            IndexModel::builder()
                .keys(doc! { "account_id": 1, "updated_at": -1 })
                .options(opts("by_account").sparse(true).build())
                .build(),
        ];
        self.database().collection::<Document>(SUPPORT_CONVERSATIONS).create_indexes(models).await?;
        Ok(())
    }

    /// The update that [`push`] makes in memory, as one atomic Mongo update:
    /// `$push` with `$slice` keeps the newest messages, the expiry moves by the
    /// conversation's own retention.
    async fn push_messages(&self, filter: Document, msgs: &[SupportMessage], extra_set: Document, now: u64) -> Result<Option<Conversation>> {
        let msgs: Vec<SupportMessage> = msgs.iter().cloned().map(|mut m| {
            m.text = clip(&m.text);
            m
        }).collect();
        let msgs = bson::to_bson(&msgs).expect("messages serialize");
        let mut set = doc! { "updated_at": now as i64 };
        set.extend(extra_set);
        let Some(c) = self
            .conversations()
            .find_one_and_update(
                filter,
                doc! {
                    "$push": { "messages": { "$each": msgs, "$slice": -(MAX_MESSAGES as i64) } },
                    "$set": set,
                },
            )
            .return_document(ReturnDocument::After)
            .await?
        else {
            return Ok(None);
        };
        let expires = now + c.retention();
        self.conversations()
            .update_one(doc! { "_id": &c.id }, doc! { "$set": { "expires_at": expires as i64, "expire_at": date(expires) } })
            .await?;
        Ok(Some(Conversation { expires_at: expires, ..c }))
    }
}

#[async_trait]
impl SupportStore for MongoStore {
    async fn start_conversation(&self, warden: &str, account_id: Option<&str>, now: u64) -> Result<(String, Conversation)> {
        let raw = random_token("rwsc_");
        let c = Conversation {
            id: hash(&raw),
            warden: warden.to_string(),
            account_id: account_id.map(str::to_string),
            messages: vec![],
            ticket: None,
            created_at: now,
            updated_at: now,
            expires_at: now + CHAT_RETENTION_SECONDS,
        };
        let mut d = to_doc(&c);
        d.insert("expire_at", date(c.expires_at));
        self.database().collection::<Document>(SUPPORT_CONVERSATIONS).insert_one(d).await?;
        Ok((raw, c))
    }

    async fn conversation(&self, raw_secret: &str, now: u64) -> Result<Option<Conversation>> {
        Ok(self
            .conversations()
            .find_one(doc! { "_id": hash(raw_secret), "expires_at": { "$gt": now as i64 } })
            .await?)
    }

    async fn append(&self, raw_secret: &str, msgs: &[SupportMessage], account_id: Option<&str>, now: u64) -> Result<Option<Conversation>> {
        let filter = doc! { "_id": hash(raw_secret), "expires_at": { "$gt": now as i64 } };
        let Some(mut c) = self.push_messages(filter, msgs, doc! {}, now).await? else { return Ok(None) };
        let reopen = c.ticket.as_ref().is_some_and(|t| t.status == TicketStatus::Answered)
            && msgs.iter().any(|m| m.from == Speaker::Visitor);
        if reopen {
            self.conversations()
                .update_one(doc! { "_id": &c.id, "ticket.status": "answered" }, doc! { "$set": { "ticket.status": "open" } })
                .await?;
            if let Some(t) = &mut c.ticket {
                t.status = TicketStatus::Open;
            }
        }
        if let (None, Some(a)) = (&c.account_id, account_id) {
            self.conversations()
                .update_one(doc! { "_id": &c.id, "account_id": { "$exists": false } }, doc! { "$set": { "account_id": a } })
                .await?;
            c.account_id = Some(a.to_string());
        }
        Ok(Some(c))
    }

    async fn open_ticket(&self, raw_secret: &str, subject: &str, email: Option<&str>, now: u64) -> Result<Option<Ticket>> {
        let id = hash(raw_secret);
        let t = Ticket {
            reference: ticket_reference(),
            status: TicketStatus::Open,
            subject: clip(subject).chars().take(140).collect(),
            email: email.map(str::to_string),
            opened_at: now,
        };
        let expires = now + TICKET_RETENTION_SECONDS;
        // Filtered on "no ticket yet", so two clicks cannot open two.
        self.conversations()
            .update_one(
                doc! { "_id": &id, "expires_at": { "$gt": now as i64 }, "ticket": { "$exists": false } },
                doc! { "$set": {
                    "ticket": bson::to_bson(&t).expect("ticket serializes"),
                    "updated_at": now as i64,
                    "expires_at": expires as i64,
                    "expire_at": date(expires),
                } },
            )
            .await?;
        Ok(self.conversations().find_one(doc! { "_id": &id }).await?.and_then(|c| c.ticket))
    }

    async fn ticket(&self, reference: &str) -> Result<Option<Conversation>> {
        Ok(self.conversations().find_one(doc! { "ticket.reference": reference }).await?)
    }

    async fn tickets(&self, q: TicketQuery) -> Result<Vec<Conversation>> {
        use futures::TryStreamExt;
        let mut filter = doc! { "ticket.reference": { "$exists": true } };
        if let Some(s) = q.status {
            filter.insert("ticket.status", s.as_str());
        }
        Ok(self
            .conversations()
            .find(filter)
            .sort(doc! { "updated_at": -1 })
            .limit(i64::from(q.limit.max(1)))
            .await?
            .try_collect()
            .await?)
    }

    async fn staff_reply(&self, reference: &str, text: &str, now: u64) -> Result<Option<Conversation>> {
        let msg = SupportMessage::new(Speaker::Staff, text, now);
        let mut c = self
            .push_messages(doc! { "ticket.reference": reference }, &[msg], doc! { "ticket.status": "answered" }, now)
            .await?;
        if let Some(t) = c.as_mut().and_then(|c| c.ticket.as_mut()) {
            t.status = TicketStatus::Answered;
        }
        Ok(c)
    }

    async fn set_ticket_status(&self, reference: &str, status: TicketStatus, now: u64) -> Result<bool> {
        let r = self
            .conversations()
            .update_one(
                doc! { "ticket.reference": reference },
                doc! { "$set": { "ticket.status": status.as_str(), "updated_at": now as i64 } },
            )
            .await?;
        Ok(r.matched_count == 1)
    }

    async fn account_tickets(&self, account_id: &str) -> Result<Vec<Conversation>> {
        use futures::TryStreamExt;
        Ok(self
            .conversations()
            .find(doc! { "account_id": account_id, "ticket.reference": { "$exists": true } })
            .sort(doc! { "updated_at": -1 })
            .limit(50)
            .await?
            .try_collect()
            .await?)
    }
}

// ---------------------------------------------------------------- contract

/// The behaviour every support store must have. Run by
/// [`crate::store::contract::run`], so the Atlas test covers it too.
pub mod contract {
    use super::*;

    const NOW: u64 = 1_793_000_000;

    pub async fn run(s: &dyn SupportStore) {
        let (raw, c) = s.start_conversation("ram", None, NOW).await.unwrap();
        assert!(raw.starts_with("rwsc_"));
        assert_ne!(c.id, raw, "only the hash is stored");
        assert!(s.conversation("rwsc_nope", NOW).await.unwrap().is_none());

        let q = SupportMessage::new(Speaker::Visitor, "  how much is Pro?  ", NOW + 1);
        let a = SupportMessage::warden("$5 every 2 months.", "pricing", NOW + 1);
        let c = s.append(&raw, &[q, a], Some("acct1"), NOW + 1).await.unwrap().unwrap();
        assert_eq!(c.messages.len(), 2);
        assert_eq!(c.messages[0].text, "how much is Pro?", "text is trimmed");
        assert_eq!(c.messages[1].topic.as_deref(), Some("pricing"));
        assert_eq!(c.account_id.as_deref(), Some("acct1"), "signing in mid-chat links it");
        assert_eq!(c.expires_at, NOW + 1 + CHAT_RETENTION_SECONDS);

        // The oldest messages fall off; a long message is clipped.
        let many: Vec<_> = (0..MAX_MESSAGES).map(|i| SupportMessage::new(Speaker::Visitor, &format!("m{i}"), NOW + 2)).collect();
        let c = s.append(&raw, &many, None, NOW + 2).await.unwrap().unwrap();
        assert_eq!(c.messages.len(), MAX_MESSAGES);
        assert_eq!(c.messages[0].text, "m0");
        let long = "x".repeat(MAX_TEXT_CHARS + 50);
        let c = s.append(&raw, &[SupportMessage::new(Speaker::Visitor, &long, NOW + 3)], None, NOW + 3).await.unwrap().unwrap();
        assert_eq!(c.messages.last().unwrap().text.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(c.messages[0].text, "m1");

        // One ticket per conversation, kept a year.
        let t = s.open_ticket(&raw, "Download failed", Some("a@example.com"), NOW + 4).await.unwrap().unwrap();
        assert!(normalise_reference(&t.reference).is_some(), "{}", t.reference);
        assert_eq!(t.status, TicketStatus::Open);
        let again = s.open_ticket(&raw, "other", None, NOW + 5).await.unwrap().unwrap();
        assert_eq!(again.reference, t.reference, "a second click returns the same ticket");
        let c = s.conversation(&raw, NOW + CHAT_RETENTION_SECONDS + 10).await.unwrap().unwrap();
        assert!(c.expires_at >= NOW + TICKET_RETENTION_SECONDS, "tickets outlive the chat window");

        // Staff reply; the visitor answering reopens it.
        let c = s.staff_reply(&t.reference, "Try the installer again now.", NOW + 6).await.unwrap().unwrap();
        assert_eq!(c.ticket.as_ref().unwrap().status, TicketStatus::Answered);
        assert_eq!(c.messages.last().unwrap().from, Speaker::Staff);
        assert!(s.staff_reply("RW-ZZZZZZ", "x", NOW).await.unwrap().is_none());
        let c = s.append(&raw, &[SupportMessage::new(Speaker::Visitor, "still broken", NOW + 7)], None, NOW + 7).await.unwrap().unwrap();
        assert_eq!(c.ticket.as_ref().unwrap().status, TicketStatus::Open);

        // Listings.
        let (raw2, _) = s.start_conversation("threat", None, NOW + 8).await.unwrap();
        assert!(s.open_ticket("rwsc_nope", "x", None, NOW).await.unwrap().is_none());
        let t2 = s.open_ticket(&raw2, "Is ThreatWarden on Linux?", None, NOW + 8).await.unwrap().unwrap();
        let all = s.tickets(TicketQuery { status: None, limit: 10 }).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].ticket.as_ref().unwrap().reference, t2.reference, "newest first");
        assert!(s.set_ticket_status(&t2.reference, TicketStatus::Closed, NOW + 9).await.unwrap());
        let open = s.tickets(TicketQuery { status: Some(TicketStatus::Open), limit: 10 }).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(s.ticket(&t.reference).await.unwrap().unwrap().id, c.id);
        let mine = s.account_tickets("acct1").await.unwrap();
        assert_eq!(mine.len(), 1);
        assert!(s.account_tickets("acct2").await.unwrap().is_empty());

        // Expired chats are gone even before the TTL monitor runs.
        let (raw3, _) = s.start_conversation("ram", None, NOW).await.unwrap();
        assert!(s.conversation(&raw3, NOW + CHAT_RETENTION_SECONDS + 1).await.unwrap().is_none());
        assert!(s.append(&raw3, &[], None, NOW + CHAT_RETENTION_SECONDS + 1).await.unwrap().is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_survive_retyping() {
        for _ in 0..100 {
            let r = ticket_reference();
            assert_eq!(normalise_reference(&r.to_lowercase()).as_deref(), Some(r.as_str()));
        }
        assert_eq!(normalise_reference("rw bcd 234").as_deref(), Some("RW-BCD234"));
        assert_eq!(normalise_reference("RW-BCD230"), None, "0 is not in the alphabet");
        assert_eq!(normalise_reference("RW-BCD2"), None);
    }
}
