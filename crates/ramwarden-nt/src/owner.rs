//! Who owns a process, and how it was started.
//!
//! Both need the process opened, which the process table itself does not, so
//! answers are cached by (pid, creation time): a process's owner and command
//! line never change, and a reused PID has a different creation time.

use std::collections::HashMap;
use std::ffi::c_void;

use ramwarden_platform::Owner;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::{
    EqualSid, GetLengthSid, GetTokenInformation, IsWellKnownSid, PSID, TOKEN_QUERY, TOKEN_USER,
    TokenUser, WinLocalServiceSid, WinLocalSystemSid, WinNetworkServiceSid,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::handle::Handle;
use crate::ntdll::{self, UnicodeString};
use crate::process::open;
use crate::snapshot::NtProcess;

/// Owner and command line for one process instance.
#[derive(Clone, Debug, Default)]
pub struct Identity {
    pub owner: Owner,
    pub cmdline: String,
}

/// Resolves and caches [`Identity`] per process instance.
pub struct Identities {
    my_sid: Vec<u8>,
    my_session: u32,
    cache: HashMap<(u32, i64), Identity>,
}

impl Default for Identities {
    fn default() -> Self {
        Self::new()
    }
}

impl Identities {
    pub fn new() -> Self {
        // SAFETY: the pseudo-handle for the current process needs no closing.
        let token_owner = unsafe { GetCurrentProcess() };
        let my_sid = token_sid(token_owner).unwrap_or_default();
        let my_session = crate::snapshot::processes()
            .ok()
            .and_then(|t| t.into_iter().find(|p| p.pid == std::process::id()))
            .map(|p| p.session)
            .unwrap_or(1);
        Identities {
            my_sid,
            my_session,
            cache: HashMap::new(),
        }
    }

    /// The interactive session this process runs in.
    pub fn session(&self) -> u32 {
        self.my_session
    }

    pub fn get(&mut self, p: &NtProcess) -> Identity {
        let key = (p.pid, p.create_time);
        if let Some(id) = self.cache.get(&key) {
            return id.clone();
        }
        let id = self.resolve(p);
        self.cache.insert(key, id.clone());
        id
    }

    /// Drop entries for processes no longer in `live`, so the cache tracks
    /// the process table rather than growing forever.
    pub fn retain(&mut self, live: &[NtProcess]) {
        let keep: std::collections::HashSet<(u32, i64)> =
            live.iter().map(|p| (p.pid, p.create_time)).collect();
        self.cache.retain(|k, _| keep.contains(k));
    }

    fn resolve(&self, p: &NtProcess) -> Identity {
        // Session 0 is services; the kernel's pseudo-processes have no token.
        if p.session == 0 || p.is_kernel() {
            return Identity {
                owner: Owner::System,
                cmdline: String::new(),
            };
        }
        // Games and their anti-cheat are never opened, not even to read an owner
        // or a command line: anti-cheat watches for foreign handles to the game,
        // and Fortnite's Easy Anti-Cheat showed "Game Security Violation Detected"
        // 86 s after a fresh daemon started resolving every process (2026-10-12).
        // Unknown is safe: a game is structurally untouchable in the ladder anyway.
        if never_open(&p.name) {
            return Identity::default();
        }
        let Ok(h) = open(p.pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
            // In our session but unopenable: protected or elevated. Treat as
            // unknown — never as ours.
            return Identity::default();
        };
        let owner = match token_sid(h.raw()) {
            Some(sid) if sid_is_system(&sid) => Owner::System,
            Some(sid) if !self.my_sid.is_empty() && sid_eq(&sid, &self.my_sid) => Owner::CurrentUser,
            Some(_) => Owner::OtherUser,
            None => Owner::Unknown,
        };
        Identity {
            owner,
            cmdline: command_line(&h).unwrap_or_default(),
        }
    }
}

/// The user SID of a process's primary token, as raw bytes.
fn token_sid(process: HANDLE) -> Option<Vec<u8>> {
    let mut raw = HANDLE::default();
    // SAFETY: `raw` receives a new token handle, owned immediately below.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut raw) }.ok()?;
    let token = Handle::new(raw)?;

    let mut len = 0u32;
    // SAFETY: sizing call; fails with ERROR_INSUFFICIENT_BUFFER by design.
    let _ = unsafe { GetTokenInformation(token.raw(), TokenUser, None, 0, &mut len) };
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: the buffer is at least `len` bytes and 8-aligned.
    unsafe {
        GetTokenInformation(
            token.raw(),
            TokenUser,
            Some(buf.as_mut_ptr() as *mut c_void),
            len,
            &mut len,
        )
    }
    .ok()?;
    // SAFETY: the kernel wrote a TOKEN_USER at the start of the buffer.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let sid = user.User.Sid;
    // SAFETY: `sid` points into `buf` and is a valid SID.
    let sid_len = unsafe { GetLengthSid(sid) } as usize;
    // SAFETY: as above; copy it out before `buf` is dropped.
    Some(unsafe { std::slice::from_raw_parts(sid.0 as *const u8, sid_len) }.to_vec())
}

fn sid_eq(a: &[u8], b: &[u8]) -> bool {
    // SAFETY: both are complete SIDs copied out of tokens.
    unsafe {
        EqualSid(PSID(a.as_ptr() as *mut c_void), PSID(b.as_ptr() as *mut c_void)).is_ok()
    }
}

fn sid_is_system(sid: &[u8]) -> bool {
    let p = PSID(sid.as_ptr() as *mut c_void);
    // SAFETY: `sid` is a complete SID.
    [WinLocalSystemSid, WinLocalServiceSid, WinNetworkServiceSid]
        .into_iter()
        .any(|kind| unsafe { IsWellKnownSid(p, kind) }.as_bool())
}

/// The command line, via `ProcessCommandLineInformation` (Windows 8.1+), which
/// works with only `PROCESS_QUERY_LIMITED_INFORMATION`.
fn command_line(h: &Handle) -> Option<String> {
    let mut len = 0u32;
    // SAFETY: sizing call with a null buffer.
    let status = unsafe {
        ntdll::NtQueryInformationProcess(
            h.raw().0,
            ntdll::PROCESS_COMMAND_LINE_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut len,
        )
    };
    if ntdll::success(status) || len == 0 || len > 64 * 1024 {
        return None;
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: buffer is `len` bytes, 8-aligned.
    let status = unsafe {
        ntdll::NtQueryInformationProcess(
            h.raw().0,
            ntdll::PROCESS_COMMAND_LINE_INFORMATION,
            buf.as_mut_ptr() as *mut c_void,
            len,
            &mut len,
        )
    };
    if !ntdll::success(status) {
        return None;
    }
    // SAFETY: the kernel wrote a UNICODE_STRING pointing into `buf`.
    Some(unsafe { (*(buf.as_ptr() as *const UnicodeString)).to_string_lossy() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_ours_and_has_a_command_line() {
        let mut ids = Identities::new();
        let table = crate::snapshot::processes().unwrap();
        let me = table.iter().find(|p| p.pid == std::process::id()).unwrap();
        let id = ids.get(me);
        assert_eq!(id.owner, Owner::CurrentUser);
        assert!(id.cmdline.to_lowercase().contains("ramwarden_nt"), "{}", id.cmdline);
    }

    #[test]
    fn session_zero_and_the_kernel_are_system() {
        let mut ids = Identities::new();
        let table = crate::snapshot::processes().unwrap();
        let system = table.iter().find(|p| p.pid == 4).unwrap();
        assert_eq!(ids.get(system).owner, Owner::System);
        if let Some(svc) = table.iter().find(|p| p.session == 0 && p.pid > 4) {
            assert_eq!(ids.get(svc).owner, Owner::System, "{}", svc.name);
        }
    }
}

/// A SID as its string form, e.g. `S-1-5-21-...-1001`.
fn sid_string(sid: &[u8]) -> Option<String> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    let mut out = windows::core::PWSTR::null();
    // SAFETY: `sid` is a complete SID; the string is LocalAlloc'd and freed here.
    unsafe {
        ConvertSidToStringSidW(PSID(sid.as_ptr() as *mut c_void), &mut out).ok()?;
        let s = out.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(out.0 as *mut c_void)));
        s
    }
}

/// Image names RamWarden must never open a handle to: Unreal game builds (the
/// `-Shipping` suffix, which covers Fortnite and its overlay) and the anti-cheat
/// services. Keep in step with `ramwarden_core::roles::ANTI_CHEAT` and `GAMES`.
pub fn never_open(name: &str) -> bool {
    const ANTI_CHEAT: &[&str] = &[
        "vgc", "vgtray", "easyanticheat", "easyanticheat_eos", "beservice",
        "eaanticheat.gameservice", "faceit", "faceitservice", "gameguard",
    ];
    const GAMES: &[&str] = &[
        "league of legends", "valorant", "cs2", "dota2", "rocketleague", "r5apex",
        "overwatch", "eldenring", "gta5", "gtav",
    ];
    let lower = name.trim().to_lowercase();
    let lower = lower.strip_suffix(".exe").unwrap_or(&lower);
    lower.ends_with("-shipping")
        || lower.starts_with("fortnite")
        || ANTI_CHEAT.contains(&lower)
        || GAMES.contains(&lower)
}

/// The user SID of the process `pid`, as a string. The service uses this to
/// find the *connecting* user's registry hive under `HKEY_USERS`.
pub fn process_user_sid(pid: u32) -> Option<String> {
    let h = open(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    sid_string(&token_sid(h.raw())?)
}

/// The SID of the user running this process.
pub fn current_user_sid() -> Option<String> {
    // SAFETY: the pseudo-handle for the current process needs no closing.
    sid_string(&token_sid(unsafe { GetCurrentProcess() })?)
}

#[cfg(test)]
mod sid_tests {
    #[test]
    fn this_process_has_a_user_sid_and_it_matches_by_pid() {
        let me = super::current_user_sid().unwrap();
        assert!(me.starts_with("S-1-5-"), "{me}");
        assert_eq!(super::process_user_sid(std::process::id()).as_deref(), Some(me.as_str()));
    }

    /// Measured on the target machine with Fortnite running.
    #[test]
    fn games_and_anti_cheat_are_never_opened() {
        for name in [
            "FortniteClient-Win64-Shipping",
            "FortniteClient-Win64-Shipping_EAC_EOS",
            "FortniteLauncher",
            "EOSOverlayRenderer-Win64-Shipping",
            "EasyAntiCheat_EOS",
            "EasyAntiCheat_EOS.exe",
            "VALORANT-Win64-Shipping",
            "vgc",
        ] {
            assert!(super::never_open(name), "{name}");
        }
        assert!(!super::never_open("Discord"));
        assert!(!super::never_open("Cursor"));
    }
}
