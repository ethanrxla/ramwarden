//! Enabling a privilege the token already holds.
//!
//! An administrator or SYSTEM token *holds* privileges such as
//! `SeProfileSingleProcessPrivilege` but has them disabled; a call that needs
//! one fails until it is enabled on the token. An ordinary user's token does
//! not hold them at all, and enabling then fails — which is the answer.

use ramwarden_platform::{Error, Result};
use windows::Win32::Foundation::{ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, GetTokenInformation, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
    SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_PRIVILEGES, TOKEN_QUERY,
    TokenElevation,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::HSTRING;

use crate::handle::Handle;

/// Needed to purge the standby list.
pub const PROFILE_SINGLE_PROCESS: &str = "SeProfileSingleProcessPrivilege";
/// Needed to open other users' processes, including protected services.
pub const DEBUG: &str = "SeDebugPrivilege";
/// Needed to change another process's working-set quota.
pub const INCREASE_QUOTA: &str = "SeIncreaseQuotaPrivilege";

/// Enable `name` on this process's token.
pub fn enable(name: &str) -> Result<()> {
    let mut raw = HANDLE::default();
    // SAFETY: `raw` receives a new token handle, owned immediately below.
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut raw)
    }
    .map_err(|e| Error::os(format!("OpenProcessToken: {e}")))?;
    let token = Handle::new(raw).ok_or_else(|| Error::os("OpenProcessToken returned no handle"))?;

    let mut luid = LUID::default();
    // SAFETY: plain lookup into a local.
    unsafe { LookupPrivilegeValueW(None, &HSTRING::from(name), &mut luid) }
        .map_err(|e| Error::os(format!("LookupPrivilegeValue({name}): {e}")))?;

    let tp = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    // SAFETY: `tp` is a complete single-entry TOKEN_PRIVILEGES.
    unsafe { AdjustTokenPrivileges(token.raw(), false, Some(&tp), 0, None, None) }
        .map_err(|e| Error::os(format!("AdjustTokenPrivileges({name}): {e}")))?;

    // AdjustTokenPrivileges "succeeds" when the token lacks the privilege, and
    // reports the truth only through the last error.
    // SAFETY: reads thread-local state.
    if unsafe { GetLastError() } == ERROR_NOT_ALL_ASSIGNED {
        return Err(Error::denied(name, "this account does not hold the privilege; run as administrator or via ramwarden-service"));
    }
    Ok(())
}

/// Whether this process runs with an elevated (administrator) token.
pub fn is_elevated() -> bool {
    let mut raw = HANDLE::default();
    // SAFETY: `raw` receives a token handle, owned immediately below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) }.is_err() {
        return false;
    }
    let Some(token) = Handle::new(raw) else { return false };
    let mut elevation = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    // SAFETY: the buffer is a TOKEN_ELEVATION of the stated size.
    unsafe {
        GetTokenInformation(
            token.raw(),
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut std::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    }
    .is_ok()
        && elevation.TokenIsElevated != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Either outcome is valid depending on how the tests run; what matters is
    /// that a missing privilege is reported as a denial, not a success.
    #[test]
    fn enabling_reports_the_truth() {
        match enable(PROFILE_SINGLE_PROCESS) {
            Ok(()) => {}
            Err(e) => assert!(e.is_denied(), "{e}"),
        }
    }
}
