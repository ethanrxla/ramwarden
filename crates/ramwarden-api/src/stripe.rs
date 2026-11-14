//! Card payments: Stripe Checkout in subscription mode with a 14-day trial
//! (spec 3.4), and the webhook that keeps the account in step.
//!
//! Prices are sent inline (`price_data`) from `contracts/plans.json`, so the
//! Stripe dashboard needs no products set up and card and crypto can never
//! disagree on a price. Checkout offers card, Apple Pay and Google Pay on its
//! own.
//!
//! The webhook is the only thing that changes an account from Stripe. Its
//! signature (`Stripe-Signature`, HMAC-SHA256 with the endpoint secret) is
//! checked before the body is parsed, and events older than five minutes are
//! refused so a captured request can't be replayed later.

use std::time::Duration;

use hmac::{Hmac, Mac};
use ramwarden_ai::secret::Secret;
use ramwarden_cloud::model::{Billing, BillingMethod, BillingUpdate, Interval, Tier};
use serde_json::Value;
use sha2::Sha256;

use crate::checkout::{PlanDef, Plans};

/// Oldest webhook accepted, by its signed timestamp.
pub const WEBHOOK_TOLERANCE: u64 = 300;
pub const TRIAL_DAYS: u32 = 14;

pub struct Stripe {
    key: Secret,
    webhook_secret: Secret,
    api_base: String,
    http: reqwest::Client,
    /// Plan id to the operator's Stripe price (`STRIPE_PRICE_PLAN_<id>`).
    prices: std::collections::BTreeMap<u32, String>,
}

pub struct CheckoutRequest<'a> {
    pub account_id: &'a str,
    /// Our payment record, confirmed when the subscription's first event
    /// arrives.
    pub payment_id: &'a str,
    pub plan: &'a PlanDef,
    pub email: Option<&'a str>,
    /// False for a plan without a trial, or an account that had its trial.
    pub trial: bool,
    pub success_url: &'a str,
    pub cancel_url: &'a str,
}

/// The recurrence of a price checkout builds itself, when the operator set
/// no Stripe price for the plan: card plans bill every 2 months or yearly
/// (user decision, 2026-11-14). A configured price carries its own.
fn stripe_interval(plan: &PlanDef) -> (&'static str, &'static str) {
    if plan.interval == "year" { ("year", "1") } else { ("month", "2") }
}

impl Stripe {
    pub fn new(key: Secret, webhook_secret: Secret) -> Self {
        Self::with_base(key, webhook_secret, "https://api.stripe.com")
    }

    /// Tests point this at a fake.
    pub fn with_base(key: Secret, webhook_secret: Secret, api_base: &str) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(15)).build().unwrap_or_default();
        Stripe { key, webhook_secret, api_base: api_base.trim_end_matches('/').to_string(), http, prices: Default::default() }
    }

    /// Use the operator's own Stripe prices, by plan id.
    pub fn with_prices(mut self, prices: std::collections::BTreeMap<u32, String>) -> Self {
        self.prices = prices;
        self
    }

    /// Whether this is a test-mode key (`sk_test_`), shown in config so the
    /// site can label test checkouts.
    pub fn test_mode(&self) -> bool {
        self.key.expose().starts_with("sk_test_") || self.key.expose().starts_with("rk_test_")
    }

    /// Creates a Checkout Session and returns its URL.
    pub async fn checkout(&self, r: &CheckoutRequest<'_>) -> Result<String, String> {
        let (interval, count) = stripe_interval(r.plan);
        let cents = r.plan.card_cents().to_string();
        let plan_id = r.plan.id.to_string();
        let trial_days = TRIAL_DAYS.to_string();
        let mut form: Vec<(&str, &str)> = vec![
            ("mode", "subscription"),
            ("client_reference_id", r.account_id),
            ("success_url", r.success_url),
            ("cancel_url", r.cancel_url),
            ("line_items[0][quantity]", "1"),
        ];
        match self.prices.get(&r.plan.id) {
            Some(price) => form.push(("line_items[0][price]", price)),
            None => form.extend([
                ("line_items[0][price_data][currency]", "usd"),
                ("line_items[0][price_data][unit_amount]", cents.as_str()),
                ("line_items[0][price_data][recurring][interval]", interval),
                ("line_items[0][price_data][recurring][interval_count]", count),
                ("line_items[0][price_data][product_data][name]", r.plan.name.as_str()),
            ]),
        }
        form.extend([
            ("subscription_data[metadata][account_id]", r.account_id),
            ("subscription_data[metadata][plan]", &plan_id),
            ("subscription_data[metadata][payment_id]", r.payment_id),
        ]);
        if r.trial {
            form.push(("subscription_data[trial_period_days]", &trial_days));
        }
        if let Some(e) = r.email {
            form.push(("customer_email", e));
        }
        let resp = self
            .http
            .post(format!("{}/v1/checkout/sessions", self.api_base))
            .basic_auth(self.key.expose(), Some(""))
            .form(&form)
            .send()
            .await
            .map_err(|e| format!("stripe: {e}"))?;
        let status = resp.status();
        let v: Value = resp.json().await.map_err(|e| format!("stripe: {e}"))?;
        if !status.is_success() {
            return Err(format!("stripe answered {status}: {}", v["error"]["message"]));
        }
        v["url"].as_str().map(str::to_string).ok_or_else(|| "stripe returned no url".into())
    }

    /// Moves a live subscription to `plan`: a new price on its one item, any
    /// trial ended, and the prorated difference invoiced now. The webhook
    /// that follows moves the tier.
    pub async fn change_plan(&self, subscription_id: &str, plan: &PlanDef, account_id: &str, payment_id: &str) -> Result<(), String> {
        if !subscription_id.starts_with("sub_") || !subscription_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err("not a subscription id".into());
        }
        let get = |path: String| self.http.get(format!("{}{path}", self.api_base)).basic_auth(self.key.expose(), Some(""));
        let post = |path: String| self.http.post(format!("{}{path}", self.api_base)).basic_auth(self.key.expose(), Some(""));
        let sub: Value = get(format!("/v1/subscriptions/{subscription_id}"))
            .send()
            .await
            .map_err(|e| format!("stripe: {e}"))?
            .json()
            .await
            .map_err(|e| format!("stripe: {e}"))?;
        let item = sub["items"]["data"][0]["id"].as_str().ok_or("stripe: subscription has no item")?.to_string();
        let price = match self.prices.get(&plan.id) {
            Some(p) => p.clone(),
            None => {
                let (interval, count) = stripe_interval(plan);
                let cents = plan.card_cents().to_string();
                let price: Value = post("/v1/prices".into())
                    .form(&[
                        ("currency", "usd"),
                        ("unit_amount", cents.as_str()),
                        ("recurring[interval]", interval),
                        ("recurring[interval_count]", count),
                        ("product_data[name]", plan.name.as_str()),
                    ])
                    .send()
                    .await
                    .map_err(|e| format!("stripe: {e}"))?
                    .json()
                    .await
                    .map_err(|e| format!("stripe: {e}"))?;
                price["id"].as_str().ok_or("stripe: no price id")?.to_string()
            }
        };
        let plan_id = plan.id.to_string();
        let resp = post(format!("/v1/subscriptions/{subscription_id}"))
            .form(&[
                ("items[0][id]", item.as_str()),
                ("items[0][price]", price.as_str()),
                ("trial_end", "now"),
                ("proration_behavior", "always_invoice"),
                ("cancel_at_period_end", "false"),
                ("metadata[account_id]", account_id),
                ("metadata[plan]", plan_id.as_str()),
                ("metadata[payment_id]", payment_id),
            ])
            .send()
            .await
            .map_err(|e| format!("stripe: {e}"))?;
        if resp.status().is_success() { Ok(()) } else { Err(format!("stripe answered {}", resp.status())) }
    }

    /// Stops renewal; the subscription runs to the end of the period it is
    /// in, as on Base (spec 3.3 step 4).
    pub async fn cancel_at_period_end(&self, subscription_id: &str) -> Result<(), String> {
        if !subscription_id.starts_with("sub_") || !subscription_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err("not a subscription id".into());
        }
        let resp = self
            .http
            .post(format!("{}/v1/subscriptions/{subscription_id}", self.api_base))
            .basic_auth(self.key.expose(), Some(""))
            .form(&[("cancel_at_period_end", "true")])
            .send()
            .await
            .map_err(|e| format!("stripe: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("stripe answered {}", resp.status()))
        }
    }

    /// Checks a `Stripe-Signature` header against the raw body.
    pub fn verify(&self, header: &str, body: &[u8], now: u64) -> bool {
        verify_signature(self.webhook_secret.expose(), header, body, now)
    }
}

pub fn verify_signature(secret: &str, header: &str, body: &[u8], now: u64) -> bool {
    let mut t = None;
    let mut sigs = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => t = v.parse::<u64>().ok(),
            Some(("v1", v)) => sigs.extend(hex::decode(v).ok()),
            _ => {}
        }
    }
    let Some(t) = t else { return false };
    if now.abs_diff(t) > WEBHOOK_TOLERANCE {
        return false;
    }
    sigs.iter().any(|sig| {
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else { return false };
        mac.update(t.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        mac.verify_slice(sig).is_ok()
    })
}

/// The payment record a subscription event confirms: one in good standing
/// that names the payment the checkout created.
pub fn confirmed_payment(event: &Value) -> Option<(&str, &str)> {
    let sub = &event["data"]["object"];
    if !event["type"].as_str()?.starts_with("customer.subscription.")
        || !matches!(sub["status"].as_str()?, "trialing" | "active")
    {
        return None;
    }
    Some((sub["metadata"]["payment_id"].as_str()?, sub["id"].as_str()?))
}

/// What a webhook event means for an account, if anything.
pub fn update_from_event(event: &Value, plans: &Plans) -> Option<(String, BillingUpdate)> {
    let kind = event["type"].as_str()?;
    if !kind.starts_with("customer.subscription.") {
        return None;
    }
    let sub = &event["data"]["object"];
    let account_id = sub["metadata"]["account_id"].as_str()?.to_string();
    let plan = plans.plan(sub["metadata"]["plan"].as_str()?.parse().ok()?)?;
    let status = sub["status"].as_str().unwrap_or("");
    // Newer API versions moved the period end onto the subscription item.
    let period_end = sub["current_period_end"]
        .as_u64()
        .or_else(|| sub["items"]["data"][0]["current_period_end"].as_u64());
    let trial_end = sub["trial_end"].as_u64();

    let mut u = BillingUpdate {
        tier: None,
        interval: None,
        billing: Some(Billing { method: BillingMethod::Card, reference: sub["id"].as_str().unwrap_or("").to_string() }),
        trial_used: trial_end.map(|_| true),
        cancelled: Some(status == "canceled" || sub["cancel_at_period_end"].as_bool() == Some(true)),
        ..Default::default()
    };
    // Paid time moves only for a subscription that is in good standing: a
    // past-due or incomplete one already shows the next period's end, which
    // nobody has paid for. Grace covers past-due.
    if matches!(status, "trialing" | "active") {
        // A failed upgrade must preserve the previously paid tier.
        u.tier = Some(if plan.tier == "pro" { Tier::Pro } else { Tier::Ram });
        u.interval = Some(if plan.interval == "year" { Interval::Year } else { Interval::Month });
        u.paid_until = period_end.max(trial_end);
    }
    if status == "trialing" {
        u.trial_ends_at = trial_end;
    } else if status == "active" {
        // A trial an upgrade ended early, or one long over.
        u.trial_ends_at = Some(trial_end.unwrap_or(0));
    }
    Some((account_id, u))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn card_recurrence_is_every_two_months_or_yearly() {
        let plans = crate::checkout::Plans::built_in();
        assert_eq!(stripe_interval(plans.plan(1).unwrap()), ("month", "2"));
        assert_eq!(stripe_interval(plans.plan(2).unwrap()), ("month", "2"));
        assert_eq!(stripe_interval(plans.plan(3).unwrap()), ("year", "1"));
    }

    pub fn sign(secret: &str, body: &[u8], t: u64) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("{t}.").as_bytes());
        mac.update(body);
        format!("t={t},v1={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn signatures_check_the_secret_the_body_and_the_age() {
        let body = br#"{"type":"x"}"#;
        let h = sign("whsec_1", body, 1000);
        assert!(verify_signature("whsec_1", &h, body, 1100));
        assert!(!verify_signature("whsec_2", &h, body, 1100), "another secret");
        assert!(!verify_signature("whsec_1", &h, br#"{"type":"y"}"#, 1100), "another body");
        assert!(!verify_signature("whsec_1", &h, body, 1000 + WEBHOOK_TOLERANCE + 1), "replayed late");
        assert!(!verify_signature("whsec_1", "v1=00", body, 1000), "no timestamp");
    }

    pub fn subscription_event(kind: &str, account: &str, plan: u32, status: &str, period_end: u64, trial_end: Option<u64>) -> Value {
        json!({"type": kind, "data": {"object": {
            "id": "sub_123", "status": status, "cancel_at_period_end": false,
            "trial_end": trial_end,
            "items": {"data": [{"current_period_end": period_end}]},
            "metadata": {"account_id": account, "plan": plan.to_string(), "payment_id": "pay1"},
        }}})
    }

    #[test]
    fn subscription_events_map_to_tier_dates_and_cancellation() {
        let plans = Plans::built_in();
        let e = subscription_event("customer.subscription.created", "a1", 2, "trialing", 2000, Some(2000));
        let (id, u) = update_from_event(&e, &plans).unwrap();
        assert_eq!(id, "a1");
        assert_eq!((u.tier, u.interval), (Some(Tier::Pro), Some(Interval::Month)));
        assert_eq!((u.paid_until, u.trial_ends_at, u.cancelled), (Some(2000), Some(2000), Some(false)));

        assert_eq!(confirmed_payment(&e), Some(("pay1", "sub_123")));

        let e = subscription_event("customer.subscription.updated", "a1", 3, "past_due", 9000, None);
        assert_eq!(confirmed_payment(&e), None, "not in good standing");
        let (_, u) = update_from_event(&e, &plans).unwrap();
        assert_eq!(u.paid_until, None, "an unpaid period is not service");
        assert_eq!(u.tier, None, "an unpaid plan change must preserve the paid tier");
        for status in ["incomplete", "past_due", "unpaid"] {
            let e = subscription_event("customer.subscription.updated", "a1", 2, status, 9000, None);
            let (_, u) = update_from_event(&e, &plans).unwrap();
            assert_eq!((u.tier, u.interval, u.paid_until), (None, None, None), "{status} must not unlock Pro");
        }

        let e = subscription_event("customer.subscription.deleted", "a1", 1, "canceled", 9000, None);
        assert_eq!(update_from_event(&e, &plans).unwrap().1.cancelled, Some(true));
        assert!(update_from_event(&json!({"type": "invoice.paid", "data": {"object": {}}}), &plans).is_none());
    }
}
