//! Explicit, brief local playback diagnostic. Never runs automatically.
fn main() -> Result<(), String> {
    let client = ramwarden_ui::client::Client::new("127.0.0.1", 7823).map_err(|e| e.to_string())?;
    for threat in [false, true] {
        ramwarden_ui::tray::audio::bleat(threat)?;
        std::thread::sleep(std::time::Duration::from_secs(2));
        ramwarden_ui::tray::audio::say(
            client.clone(),
            threat,
            if threat {
                "ThreatWarden here. Your voice connection is working."
            } else {
                "RamWarden here. Your voice connection is working."
            }
            .into(),
            true,
        )
        .recv_timeout(std::time::Duration::from_secs(50))
        .map_err(|e| e.to_string())??;
        println!(
            "{} playback completed",
            if threat { "ThreatWarden" } else { "RamWarden" }
        );
    }
    Ok(())
}
