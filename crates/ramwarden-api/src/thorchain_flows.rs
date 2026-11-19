//! The THORChain checkout end to end against a fake THORNode and Midgard:
//! quote, payment, the swap the watcher finds, and the period it grants.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::routing::get;
use ramwarden_cloud::store::Store;
use serde_json::{Value, json};

use crate::checkout::Plans;
use crate::flows::{App, Auth, NOW, Opts, app_with};
use crate::thorchain::tests::{PAYOUT, action, cfg};
use crate::thorchain::{Thorchain, memo_limit};
use crate::wallet::testing::EvmWallet;

const DAY: u64 = 86_400;

type Actions = Arc<Mutex<Vec<Value>>>;

/// BTC at $90,000 and ETH at $3,000; a 25-cent outbound fee on every swap.
async fn fake_thorchain() -> (String, Actions) {
    let actions: Actions = Arc::default();
    let rows = actions.clone();
    let price = |asset: &str| if asset.starts_with("BTC") { 9_000_000_000_000u64 } else { 300_000_000_000 };
    let app = axum::Router::new()
        .route(
            "/thorchain/pool/{asset}",
            get(move |Path(asset): Path<String>| async move {
                Json(json!({"status": "Available", "asset_tor_price": price(&asset).to_string()}))
            }),
        )
        .route(
            "/thorchain/quote/swap",
            get(move |Query(q): Query<std::collections::HashMap<String, String>>| async move {
                let amount: u64 = q["amount"].parse().unwrap();
                assert_eq!(q["to_asset"], crate::thorchain::PAYOUT_ASSET);
                assert_eq!(q["destination"], PAYOUT);
                let out = (u128::from(amount) * u128::from(price(&q["from_asset"])) / 100_000_000) as u64 - 25_000_000;
                let evm = q["from_asset"].starts_with("ETH");
                Json(json!({
                    "inbound_address": if evm { "0x489db109a54f37ffe5464b86c0668ce84dd9cb02" } else { "bc1qvault" },
                    "router": evm.then_some("0xD37BbE5744D730a1d98d8DC97c42F0Ca46aD7146"),
                    "expiry": NOW + 600,
                    "fees": {"outbound": "25000000"},
                    "expected_amount_out": out.to_string(),
                    "total_swap_seconds": 600,
                }))
            }),
        )
        .route(
            "/v2/actions",
            get(move || {
                let a = rows.lock().unwrap().clone();
                async move { Json(json!({"actions": a, "meta": {"nextPageToken": ""}})) }
            }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, actions)
}

async fn signed_in(live: bool) -> (App, String, String, String, Actions) {
    let (url, actions) = fake_thorchain().await;
    let a = app_with(Opts { thorchain: Some(cfg(&url, live)), ..Default::default() });
    let (_, cookie, account_id) = a.sign_in_device(&EvmWallet::new(11)).await;
    (a, cookie, account_id, url, actions)
}

#[tokio::test]
async fn without_the_live_flag_it_only_quotes() {
    let (a, cookie, _, _, _) = signed_in(false).await;
    let (_, c) = a.call("GET", "/v1/checkout/config", Auth::None, None).await;
    assert_eq!(c["thorchain"]["live"], false);
    assert!(c["thorchain"]["assets"].as_array().unwrap().iter().any(|x| x["asset"] == "BTC.BTC"));

    let (s, q) = a.call("POST", "/v1/checkout/thorchain", Auth::Cookie(&cookie), Some(json!({"plan": 1, "asset": "BTC.BTC"}))).await;
    assert_eq!(s, StatusCode::OK, "{q}");
    assert_eq!(q["preview"], true);
    assert!(q.get("inbound_address").is_none() && q.get("memo").is_none(), "no deposit address without real payments: {q}");
    assert_eq!(q["renews"], false);
}

#[tokio::test]
async fn a_bitcoin_swap_buys_one_period_once() {
    let (a, cookie, account_id, url, actions) = signed_in(true).await;
    let (s, q) = a.call("POST", "/v1/checkout/thorchain", Auth::Cookie(&cookie), Some(json!({"plan": 1, "asset": "btc.btc"}))).await;
    assert_eq!(s, StatusCode::OK, "{q}");
    let (memo, pid) = (q["memo"].as_str().unwrap().to_string(), q["payment_id"].as_str().unwrap().to_string());
    let limit = memo_limit(&memo, PAYOUT).unwrap();
    assert_eq!(pid, format!("thor-{limit}"));
    let price = Plans::built_in().plan(1).unwrap().usd * 100_000_000;
    assert!(limit >= price + 25_000_000, "the limit covers the price and the outbound fee");
    assert_eq!(q["inbound_address"], "bc1qvault");
    assert_eq!(q["transactions"].as_array().unwrap().len(), 0, "Bitcoin is sent by hand, with the memo");
    let amount: f64 = q["amount"].as_str().unwrap().parse().unwrap();
    assert!(amount * 90_000.0 * 1e8 >= limit as f64, "the quoted amount pays at least the limit: {amount}");

    // Nothing granted until the swap is seen.
    let watcher = Thorchain::new(cfg(&url, true));
    let plans = Plans::built_in();
    assert_eq!(watcher.poll(a.store.as_ref(), &plans, NOW).await.unwrap(), 0);
    assert_eq!(a.store.payment(&pid).await.unwrap().unwrap().status, ramwarden_cloud::model::PaymentStatus::Pending);

    actions.lock().unwrap().push(action(&memo, PAYOUT, limit + 3_000_000, 100, "BTCIN1"));
    assert_eq!(watcher.poll(a.store.as_ref(), &plans, NOW).await.unwrap(), 1);
    let acct = a.store.account(&account_id).await.unwrap().unwrap();
    assert_eq!(acct.paid_until, Some(NOW + 30 * DAY));
    let (_, v) = a.call("GET", "/v1/account", Auth::Cookie(&cookie), None).await;
    assert_eq!((v["tier"].as_str(), v["billing"]["method"].as_str()), (Some("ram"), Some("thorchain")));
    assert_eq!(v["status"], "cancelled", "prepaid: shown as ending, not renewing");

    // A rescan grants nothing more.
    let again = Thorchain::new(cfg(&url, true));
    assert_eq!(again.poll(a.store.as_ref(), &plans, NOW).await.unwrap(), 0);
    assert_eq!(a.store.account(&account_id).await.unwrap().unwrap().paid_until, Some(NOW + 30 * DAY));

    // Paying again extends from the end of the paid time; Ether goes through the router.
    let (s, q) = a.call("POST", "/v1/checkout/thorchain", Auth::Cookie(&cookie), Some(json!({"plan": 1, "asset": "ETH.ETH"}))).await;
    assert_eq!(s, StatusCode::OK, "{q}");
    let tx = &q["transactions"][0];
    assert_eq!(tx["to"], "0xD37BbE5744D730a1d98d8DC97c42F0Ca46aD7146");
    assert!(tx["data"].as_str().unwrap().contains(&hex::encode(q["memo"].as_str().unwrap())));
    assert_eq!(q["chain_id"], 1);
    let limit2 = memo_limit(q["memo"].as_str().unwrap(), PAYOUT).unwrap();
    actions.lock().unwrap().push(action(q["memo"].as_str().unwrap(), PAYOUT, limit2, 101, "ETHIN"));
    assert_eq!(again.poll(a.store.as_ref(), &plans, NOW + DAY).await.unwrap(), 1);
    assert_eq!(a.store.account(&account_id).await.unwrap().unwrap().paid_until, Some(NOW + 60 * DAY));
}

#[tokio::test]
async fn a_short_swap_grants_nothing_and_an_unknown_coin_is_refused() {
    let (a, cookie, account_id, url, actions) = signed_in(true).await;
    let (s, _) = a.call("POST", "/v1/checkout/thorchain", Auth::Cookie(&cookie), Some(json!({"plan": 2, "asset": "ETH.USDT"}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, q) = a.call("POST", "/v1/checkout/thorchain", Auth::Cookie(&cookie), Some(json!({"plan": 2, "asset": "BTC.BTC"}))).await;
    let memo = q["memo"].as_str().unwrap();
    actions.lock().unwrap().push(action(memo, PAYOUT, 100_000_000, 7, "SHORT"));
    assert_eq!(Thorchain::new(cfg(&url, true)).poll(a.store.as_ref(), &Plans::built_in(), NOW).await.unwrap(), 0);
    assert_eq!(a.store.account(&account_id).await.unwrap().unwrap().tier, None);
}
