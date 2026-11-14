//! Read-only local diagnostic. Connection addresses are omitted unless requested.
#[tokio::main]
async fn main() {
    let mut report = ramwarden_wazuh::sensors::collect().await;
    let count = report.network.connections.len();
    if !std::env::args().any(|s| s == "--connections") {
        report.network.connections.clear();
    }
    println!(
        "{}",
        serde_json::json!({"connection_count": count, "report": report})
    );
}
