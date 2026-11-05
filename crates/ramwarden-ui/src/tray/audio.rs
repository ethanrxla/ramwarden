//! Same daemon-generated voices as Mac, with bounded native Windows playback.
use crate::{client::Client, companions::Options};
use std::{
    io::Write,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};
static PLAYBACK_ID: AtomicU64 = AtomicU64::new(0);
use windows::{
    Win32::Media::{Audio::*, Multimedia::*},
    core::{HSTRING, PCWSTR},
};
pub fn say(
    client: Client,
    threat: bool,
    text: String,
    test: bool,
) -> mpsc::Receiver<Result<(), String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = (|| -> Result<(), String> {
            let options = Options::load();
            let style = if threat { options.threat } else { options.ram };
            if style.audio_muted {
                return Err("Companion audio is muted".into());
            }
            if !test && !style.spoken_replies {
                return Ok(());
            }
            let rt = crate::runtime::network_runtime().map_err(|e| e.to_string())?;
            let bytes = rt
                .block_on(client.speech_audio(&text, if threat { "threat" } else { "ram" }))
                .map_err(|e| e.to_string())?;
            let options = Options::load();
            if (if threat { options.threat } else { options.ram }).audio_muted {
                return Ok(());
            }
            let mut file = tempfile::Builder::new()
                .prefix("ramwarden-voice-")
                .suffix(".mp3")
                .tempfile()
                .map_err(|e| e.to_string())?;
            file.write_all(&bytes).map_err(|e| e.to_string())?;
            file.flush().map_err(|e| e.to_string())?;
            let temporary_path = file.into_temp_path();
            let path = temporary_path.to_string_lossy();
            let alias = format!(
                "ramwarden_voice_{}",
                PLAYBACK_ID.fetch_add(1, Ordering::Relaxed)
            );
            // SAFETY: filenames are generated locally, not model output. Only the fixed
            // MCI open/play/close verbs are used; model text is never a playback command.
            unsafe {
                mci(&format!("open \"{path}\" type mpegvideo alias {alias}"))?;
                let played = mci(&format!("play {alias} wait"));
                let _ = mci(&format!("close {alias}"));
                played?;
            }
            Ok(())
        })();
        let _ = tx.send(r);
    });
    rx
}
unsafe fn mci(command: &str) -> Result<(), String> {
    let text = HSTRING::from(command);
    let code = unsafe { mciSendStringW(&text, None, None) };
    if code == 0 {
        Ok(())
    } else {
        let mut message = [0u16; 256];
        let described = unsafe { mciGetErrorStringW(code, &mut message) }.as_bool();
        let detail = if described {
            String::from_utf16_lossy(
                &message[..message
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(message.len())],
            )
        } else {
            "Unknown playback error".into()
        };
        Err(format!("Windows audio playback failed ({code}): {detail}"))
    }
}
pub fn bleat(threat: bool) -> Result<(), String> {
    let options = Options::load();
    if (if threat { options.threat } else { options.ram }).audio_muted {
        return Err("Companion audio is muted".into());
    }
    let bytes = include_bytes!("../../../../assets/audio/baa.wav");
    // SAFETY: WinMM reads an embedded WAV with static lifetime; no model data.
    let ok = unsafe {
        PlaySoundW(
            PCWSTR(bytes.as_ptr().cast()),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        )
    };
    if ok.as_bool() {
        Ok(())
    } else {
        Err("Windows could not play the companion bleat".into())
    }
}
