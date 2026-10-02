//! The WebSocket the Chrome and Brave extensions connect to.
//!
//! The protocol is a fixed contract — the extensions are already installed:
//!
//! ```text
//! daemon -> extension   {"action":"get_tabs"}
//!                       {"action":"close","tabIds":[1,2]}
//!                       {"action":"ping"}
//! extension -> daemon   {"action":"tab_report","tabs":[...]}
//!                       {"action":"tabs_closed","tabIds":[1,2]}
//!                       {"action":"pong"}
//!                       {"action":"ping"}     <- yes, both directions
//! ```
//!
//! The extension sends its own keepalive `ping`, not only a `pong` in reply to
//! ours. v1 ignored it silently; this answers it, which costs nothing and keeps
//! the extension's own liveness check honest.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::hub::AppState;
use crate::tabs::Tab;

/// How often to ping an idle socket, so a dead connection is noticed.
const PING_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
#[serde(tag = "action")]
enum ClientMessage {
    #[serde(rename = "tab_report")]
    TabReport {
        #[serde(default)]
        tabs: Vec<Tab>,
    },
    #[serde(rename = "tabs_closed")]
    TabsClosed {
        #[serde(default, rename = "tabIds")]
        tab_ids: Vec<i64>,
    },
    #[serde(rename = "tabs_discarded")]
    TabsDiscarded {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(default, rename = "tabIds")]
        tab_ids: Vec<i64>,
    },
    #[serde(rename = "pong")]
    Pong,
    /// The extension's own keepalive.
    #[serde(rename = "ping")]
    Ping,
}

pub async fn handler(ws: WebSocketUpgrade, State(st): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| connection(socket, st))
}

async fn connection(socket: WebSocket, st: AppState) {
    let conn_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    {
        let mut hub = st.hub.lock().unwrap();
        hub.attach(&conn_id, tx);
        hub.send(&conn_id, &serde_json::json!({"action":"get_tabs"}));
        tracing::info!(
            "extension connected [{conn_id}] — {} browser(s) connected",
            hub.reg.browsers_connected()
        );
    }

    // One task owns the sink: outbound pushes and the keepalive both go through
    // the channel, so nothing contends for the socket's write half.
    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(PING_INTERVAL);
        ping.tick().await; // the first tick is immediate
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(text) => {
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                _ = ping.tick() => {
                    let ping = serde_json::json!({"action": "ping"}).to_string();
                    if sink.send(Message::Text(ping.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(text) = msg else {
            // Binary frames, pings and closes need no handling: the extension
            // only ever sends JSON text.
            continue;
        };
        match serde_json::from_str::<ClientMessage>(&text) {
            Ok(ClientMessage::TabReport { tabs }) => {
                tracing::debug!("[{conn_id}] reported {} tabs", tabs.len());
                st.hub.lock().unwrap().deliver_tabs(&conn_id, tabs);
            }
            Ok(ClientMessage::TabsClosed { tab_ids }) => {
                tracing::info!("[{conn_id}] confirmed {} tab(s) closed", tab_ids.len());
                st.hub.lock().unwrap().deliver_close(&conn_id, tab_ids);
            }
            Ok(ClientMessage::TabsDiscarded { request_id, tab_ids }) => {
                st.hub.lock().unwrap().deliver_discard(&conn_id, &request_id, tab_ids);
            }
            Ok(ClientMessage::Pong) => {}
            Ok(ClientMessage::Ping) => {
                // Answer through the writer task, so nothing else contends for
                // the socket's write half.
                st.hub
                    .lock()
                    .unwrap()
                    .send(&conn_id, &serde_json::json!({"action": "pong"}));
            }
            Err(e) => tracing::debug!("[{conn_id}] unparseable message: {e}"),
        }
    }

    writer.abort();
    st.hub.lock().unwrap().detach(&conn_id);
    tracing::info!("extension disconnected [{conn_id}]");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Option<ClientMessage> {
        serde_json::from_str(s).ok()
    }

    #[test]
    fn parses_a_tab_report_in_the_shape_the_extension_sends() {
        let msg = parse(
            r#"{"action":"tab_report","tabs":[
                 {"id":1,"url":"https://x/","title":"X","inactiveMinutes":90,"incognito":false}]}"#,
        )
        .unwrap();
        match msg {
            ClientMessage::TabReport { tabs } => {
                assert_eq!(tabs.len(), 1);
                assert_eq!(tabs[0].inactive_minutes, 90);
            }
            _ => panic!("wrong variant"),
        }
    }

    /// The extension spells it `tabIds`.
    #[test]
    fn parses_a_close_confirmation() {
        match parse(r#"{"action":"tabs_closed","tabIds":[4,5,6]}"#).unwrap() {
            ClientMessage::TabsClosed { tab_ids } => assert_eq!(tab_ids, vec![4, 5, 6]),
            _ => panic!("wrong variant"),
        }
    }

    /// The extension pings us too. v1 logged this as an unknown action on every
    /// keepalive.
    #[test]
    fn parses_the_extensions_own_ping() {
        assert!(matches!(
            parse(r#"{"action":"ping"}"#).unwrap(),
            ClientMessage::Ping
        ));
    }

    #[test]
    fn parses_a_pong() {
        assert!(matches!(
            parse(r#"{"action":"pong"}"#).unwrap(),
            ClientMessage::Pong
        ));
    }

    #[test]
    fn an_empty_tab_report_is_valid() {
        match parse(r#"{"action":"tab_report"}"#).unwrap() {
            ClientMessage::TabReport { tabs } => assert!(tabs.is_empty()),
            _ => panic!("wrong variant"),
        }
    }

    /// Firefox's extension context probes this port with non-WebSocket requests,
    /// and a future extension may send actions this build does not know. Neither
    /// should be treated as an error worth logging loudly.
    #[test]
    fn an_unknown_action_is_ignored_rather_than_fatal() {
        assert!(parse(r#"{"action":"something_new"}"#).is_none());
        assert!(parse("not json at all").is_none());
        assert!(parse("").is_none());
    }
}
