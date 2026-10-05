//! Windows, focus and audio — the signals that say the user is *using* an app.
//!
//! The Windows counterpart of the Linux `pactl`/`wmctrl` probe, without the
//! subprocesses: windows come from `EnumWindows`, focus from
//! `GetForegroundWindow`, and audio from WASAPI's session enumerator, which
//! names the process behind every active stream on every endpoint.
//!
//! Cached on the same clocks as the Linux probe, because the audio walk touches
//! every endpoint and is not free.

use std::cell::Cell;
use std::collections::HashSet;
use std::ffi::c_void;
use std::time::{Duration, Instant};

use ramwarden_platform::{Desktop, DesktopSource};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, MMDeviceEnumerator, eAll,
};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetForegroundWindow, GetWindow, GetWindowThreadProcessId, IsWindowVisible,
};
use windows::core::{BOOL, Interface};

pub const AUDIO_REFRESH: Duration = Duration::from_secs(15);
pub const WINDOW_REFRESH: Duration = Duration::from_secs(5);

/// Caches window and audio signals, each refreshed on its own clock.
#[derive(Default)]
pub struct NtDesktop {
    audio: HashSet<i32>,
    audio_at: Option<Instant>,
    windows: HashSet<i32>,
    focused: Option<i32>,
    windows_at: Option<Instant>,
}

impl NtDesktop {
    pub fn new() -> Self {
        Self::default()
    }
}

impl DesktopSource for NtDesktop {
    fn sample(&mut self) -> Desktop {
        let stale = |at: Option<Instant>, ttl: Duration| at.is_none_or(|t| t.elapsed() > ttl);
        if stale(self.audio_at, AUDIO_REFRESH) {
            self.audio = audio_pids();
            self.audio_at = Some(Instant::now());
        }
        if stale(self.windows_at, WINDOW_REFRESH) {
            self.windows = window_pids();
            self.focused = focused_pid();
            self.windows_at = Some(Instant::now());
        }
        Desktop {
            audio_pids: self.audio.clone(),
            window_pids: self.windows.clone(),
            focused_pid: self.focused,
        }
    }
}

fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: plain query on a window handle the system gave us.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// A window a user would recognise as an app window: visible, top-level,
/// unowned, and not cloaked (UWP frames and other virtual desktops cloak).
fn is_app_window(hwnd: HWND) -> bool {
    // SAFETY: queries on a window handle the system gave us.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|h| !h.0.is_null()) {
            return false;
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        );
        cloaked == 0
    }
}

/// PIDs owning an app window.
pub fn window_pids() -> HashSet<i32> {
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the &mut HashSet passed below, alive for the call.
        let out = unsafe { &mut *(lparam.0 as *mut HashSet<i32>) };
        if is_app_window(hwnd) {
            let pid = pid_of(hwnd);
            if pid > 4 {
                out.insert(pid as i32);
            }
        }
        BOOL(1)
    }
    let mut out = HashSet::new();
    // SAFETY: `out` outlives EnumWindows, which calls back synchronously.
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut out as *mut HashSet<i32> as isize));
    }
    out
}

/// PID owning the foreground window.
pub fn focused_pid() -> Option<i32> {
    // SAFETY: plain query.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return None;
    }
    match pid_of(hwnd) {
        0..=4 => None,
        pid => Some(pid as i32),
    }
}

thread_local! {
    static COM_READY: Cell<bool> = const { Cell::new(false) };
}

/// COM once per thread, multithreaded apartment. Never uninitialised: the
/// probe lives as long as the thread.
fn ensure_com() {
    COM_READY.with(|ready| {
        if !ready.get() {
            // SAFETY: initialising COM on this thread; S_FALSE (already
            // initialised) and RPC_E_CHANGED_MODE are both usable states.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            ready.set(true);
        }
    });
}

/// PIDs with an active audio session — playing or capturing — on any device.
pub fn audio_pids() -> HashSet<i32> {
    ensure_com();
    collect_audio().unwrap_or_default()
}

fn collect_audio() -> windows::core::Result<HashSet<i32>> {
    let mut out = HashSet::new();
    // SAFETY: COM calls on interfaces obtained from the system enumerator.
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = enumerator.EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE)?;
        for i in 0..devices.GetCount()? {
            let Ok(device) = devices.Item(i) else { continue };
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                continue;
            };
            let Ok(sessions) = manager.GetSessionEnumerator() else { continue };
            for j in 0..sessions.GetCount().unwrap_or(0) {
                let Ok(session) = sessions.GetSession(j) else { continue };
                if session.GetState().ok() != Some(AudioSessionStateActive) {
                    continue;
                }
                let Ok(control) = session.cast::<IAudioSessionControl2>() else { continue };
                if let Ok(pid) = control.GetProcessId()
                    && pid > 4
                {
                    out.insert(pid as i32);
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever is on screen, sampling must not fail and must never name the
    /// kernel.
    #[test]
    fn sampling_the_live_desktop_is_safe() {
        let mut d = NtDesktop::new();
        let s = d.sample();
        assert!(!s.window_pids.contains(&4) && !s.window_pids.contains(&0));
        assert!(s.focused_pid.is_none_or(|p| p > 4));
        assert!(!s.audio_pids.contains(&0));
    }
}
