//! The site wardens' support chat end to end, on the same harness as
//! `flows.rs`. Jev is unreachable here (port 9), so every answer comes
//! through the keyword router: the path a visitor gets when the budget is
//! spent or Jev is down, which is the one that must never break.

use axum::http::StatusCode;
use ramwarden_cloud::model::{BillingUpdate, Interval, Tier};
use ramwarden_cloud::store::Store;
use ramwarden_cloud::support::{SupportStore, TicketStatus};
use serde_json::{Value, json};

use crate::flows::{App, Auth, NOW, ORIGIN, STAFF, app};
use crate::wallet::testing::EvmWallet;

impl App {
    async fn ask(&self, conversation: Option<&str>, message: &str) -> Value {
        let body = json!({"conversation": conversation, "warden": "ram", "message": message});
        let (s, v) = self.call("POST", "/v1/support/chat", Auth::None, Some(body)).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }
}

#[tokio::test]
async fn a_visitor_asks_about_price_then_opens_a_ticket_the_team_answers() {
    let a = app();

    // The chat opens with a greeting and starter chips, and nothing stored.
    let (s, h) = a.call("POST", "/v1/support/history", Auth::None, Some(json!({"warden": "ram"}))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["greeting"].as_str().unwrap().contains("warden"));
    assert_eq!(h["live"], false);
    assert!(h["chips"].as_array().unwrap().iter().any(|c| c["topic"] == "trial"));

    // A price question gets the bank's answer and a secret to continue with.
    let v = a.ask(None, "How much is Pro?").await;
    let secret = v["conversation"].as_str().unwrap().to_string();
    assert!(secret.starts_with("rwsc_"));
    assert_eq!(v["reply"]["topic"], "pricing");
    assert!(v["reply"]["text"].as_str().unwrap().contains("Pro is $5 every 2 months or $24 a year"));
    assert_eq!(v["reply"]["offer_ticket"], false);

    // The same conversation continues; no new secret is handed out.
    let v = a.ask(Some(&secret), "my download is not working").await;
    assert!(v["conversation"].is_null());
    assert_eq!(v["reply"]["topic"], "human");
    assert_eq!(v["reply"]["offer_ticket"], true);

    // A chip is answered without routing.
    let body = json!({"conversation": secret, "warden": "ram", "topic": "trial"});
    let (_, v) = a.call("POST", "/v1/support/chat", Auth::None, Some(body)).await;
    assert_eq!(v["reply"]["topic"], "trial");

    // Something it does not know: it says so and offers a ticket.
    let v = a.ask(Some(&secret), "zxqv flarb").await;
    assert_eq!(v["reply"]["topic"], "unknown");
    assert_eq!(v["reply"]["offer_ticket"], true);

    // Ticket, once.
    let body = json!({"conversation": secret, "email": "Visitor@Example.com"});
    let (s, t) = a.call("POST", "/v1/support/ticket", Auth::None, Some(body.clone())).await;
    assert_eq!(s, StatusCode::OK, "{t}");
    let reference = t["ticket"]["reference"].as_str().unwrap().to_string();
    assert_eq!(t["ticket"]["subject"], "zxqv flarb", "the last question is the subject");
    assert!(t["reply"]["text"].as_str().unwrap().contains("visitor@example.com"));
    let (_, again) = a.call("POST", "/v1/support/ticket", Auth::None, Some(body)).await;
    assert_eq!(again["ticket"]["reference"], reference.as_str());
    let (s, _) = a
        .call("POST", "/v1/support/ticket", Auth::None, Some(json!({"conversation": secret, "email": "nope"})))
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // Staff need their token.
    let (s, _) = a.call("GET", "/v1/support/staff/tickets", Auth::None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = a.call("GET", "/v1/support/staff/tickets", Auth::Bearer("rwstaff_wrong_token_0123456789"), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, q) = a.call("GET", "/v1/support/staff/tickets?status=open", Auth::Bearer(STAFF), None).await;
    assert_eq!(s, StatusCode::OK, "{q}");
    assert_eq!(q["tickets"][0]["ticket"]["reference"], reference.as_str());
    assert_eq!(q["tickets"][0]["ticket"]["email"], "visitor@example.com");
    assert!(q["tickets"][0]["topics"].as_array().unwrap().iter().any(|t| t == "pricing"));

    // The team answers; the visitor sees it when the chat opens again.
    let lower = reference.to_lowercase();
    let (s, r) = a
        .call("POST", &format!("/v1/support/staff/tickets/{lower}/reply"), Auth::Bearer(STAFF), Some(json!({"text": "Fixed, try again."})))
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["ticket"]["status"], "answered");
    let (_, h) = a.call("POST", "/v1/support/history", Auth::None, Some(json!({"conversation": secret}))).await;
    assert_eq!(h["live"], true);
    let msgs = h["thread"]["messages"].as_array().unwrap();
    assert_eq!(msgs.last().unwrap()["from"], "staff");
    assert_eq!(msgs.last().unwrap()["text"], "Fixed, try again.");
    assert!(h["thread"]["ticket"].get("email").is_none(), "the visitor view does not echo the address");

    let (s, _) = a
        .call("POST", &format!("/v1/support/staff/tickets/{reference}/status"), Auth::Bearer(STAFF), Some(json!({"status": "closed"})))
        .await;
    assert_eq!(s, StatusCode::OK);
    let c = a.store.ticket(&reference).await.unwrap().unwrap();
    assert_eq!(c.ticket.unwrap().status, TicketStatus::Closed);
}

#[tokio::test]
async fn a_signed_in_visitor_hears_their_own_plan_and_sees_their_tickets() {
    let a = app();
    let (cookie, id) = a.web(&EvmWallet::new(7)).await;
    a.store
        .update_billing(
            &id,
            &BillingUpdate {
                tier: Some(Tier::Ram),
                interval: Some(Interval::TwoMonths),
                trial_ends_at: Some(NOW + 10 * 86_400),
                paid_until: Some(NOW + 10 * 86_400),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let body = json!({"warden": "ram", "message": "what plan am I on?"});
    let (s, v) = a.call("POST", "/v1/support/chat", Auth::Cookie(&cookie), Some(body)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let text = v["reply"]["text"].as_str().unwrap();
    assert!(text.starts_with("You're on RamWarden Plus: in your free trial until"), "{text}");
    assert!(text.contains("0 of 3 computers"), "{text}");

    // Signed out, the same question gets the generic answer.
    let v = a.ask(None, "what plan am I on?").await;
    assert!(v["reply"]["text"].as_str().unwrap().starts_with("Sign in"));

    // A wrong-origin cookie is treated as signed out, not as an error.
    let body = json!({"warden": "ram", "message": "what plan am I on?"});
    let (s, v, _) = a.raw("POST", "/v1/support/chat", Auth::Cookie(&cookie), "https://evil.example", Some(body)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["reply"]["text"].as_str().unwrap().starts_with("Sign in"));

    // Tickets follow the account.
    let secret = {
        let (_, v) = a
            .call("POST", "/v1/support/chat", Auth::Cookie(&cookie), Some(json!({"message": "I want a refund"})))
            .await;
        v["conversation"].as_str().unwrap().to_string()
    };
    let (s, _) = a.call("POST", "/v1/support/ticket", Auth::Cookie(&cookie), Some(json!({"conversation": secret}))).await;
    assert_eq!(s, StatusCode::OK);
    let (s, mine) = a.call("GET", "/v1/support/tickets", Auth::Cookie(&cookie), None).await;
    assert_eq!(s, StatusCode::OK, "{mine}");
    assert_eq!(mine["tickets"].as_array().unwrap().len(), 1);
    let (s, _) = a.call("GET", "/v1/support/tickets", Auth::None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let _ = ORIGIN;
}

#[tokio::test]
async fn the_chat_refuses_empty_huge_and_too_fast() {
    let a = app();
    for body in [json!({"message": "   "}), json!({}), json!({"message": "x".repeat(2001)})] {
        let (s, _) = a.call("POST", "/v1/support/chat", Auth::None, Some(body)).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let v = a.ask(None, "hi").await;
    let secret = v["conversation"].as_str().unwrap().to_string();
    for _ in 1..crate::support::PER_MINUTE {
        a.ask(Some(&secret), "how much").await;
    }
    let body = json!({"conversation": secret, "message": "how much"});
    let (s, v) = a.call("POST", "/v1/support/chat", Auth::None, Some(body)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{v}");
    // An expired or invented secret starts a fresh conversation instead.
    let v = a.ask(Some("rwsc_invented"), "how much").await;
    assert!(v["conversation"].as_str().unwrap().starts_with("rwsc_"));
}
