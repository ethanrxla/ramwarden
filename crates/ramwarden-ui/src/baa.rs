//! The warden's baa on Linux.
//!
//! macOS plays it with `NSSound` and Windows with `PlaySoundW`. Linux has no
//! one audio API a GTK app can assume, so the clip is handed to whichever
//! player the desktop already ships: PipeWire's, PulseAudio's, then ALSA's.
//! None installed means a silent alert: the bubble still shows, which is the
//! normal fallback, not an error.
//!
//! The daemon decides when a baa is due and rations it
//! (`ramwarden_core::storage::Watch`); this only plays what it is told to.

use std::io::Write;
use std::process::{Command, Stdio};

const BAA: &[u8] = include_bytes!("../../../assets/audio/baa.wav");
const BAA_CRITICAL: &[u8] = include_bytes!("../../../assets/audio/baa-critical.wav");
/// Tried in order; the first that starts wins.
const PLAYERS: &[(&str, &[&str])] = &[("pw-play", &[]), ("paplay", &[]), ("aplay", &["-q"])];

/// Play the clip without blocking the UI. Returns whether a player started.
pub fn play(critical: bool) -> bool {
    let clip = if critical { BAA_CRITICAL } else { BAA };
    // The players read a file, not stdin, in every version; the clip is
    // written once per session to a private temp file.
    let Ok(mut file) = tempfile::Builder::new().prefix("ramwarden-baa-").suffix(".wav").tempfile() else { return false };
    if file.write_all(clip).and_then(|()| file.flush()).is_err() {
        return false;
    }
    for (player, args) in PLAYERS {
        let spawned = Command::new(player).args(*args).arg(file.path()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        if let Ok(mut child) = spawned {
            // Reap the player and remove the clip when it finishes.
            std::thread::spawn(move || {
                let _ = child.wait();
                drop(file);
            });
            return true;
        }
    }
    false
}
