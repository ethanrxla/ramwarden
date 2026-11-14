//! Temporary native-pet fixture, using the real local entitlement read only.
use ramwarden_ui::{client::Client, tray::pets::Pets};
use std::time::{Duration, Instant};
use windows::Win32::UI::WindowsAndMessaging::*;
fn main() {
    let runtime = ramwarden_ui::runtime::network_runtime().unwrap();
    let client = Client::new("127.0.0.1", 7823).unwrap();
    let pro = runtime.block_on(client.threat_pane()).unlocked;
    let pets = Pets::preview().unwrap();
    pets.refresh(pro);
    println!("Native pets visible; ThreatWarden entitlement: {pro}");
    let deadline = Instant::now()
        + Duration::from_secs(
            std::env::args()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(180),
        );
    while Instant::now() < deadline {
        // SAFETY: dispatch messages only for the fixture's own UI thread.
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if pets.click().is_some() {println!("companion click received");}
        std::thread::sleep(Duration::from_millis(25));
    }
}
