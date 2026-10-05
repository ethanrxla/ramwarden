//! The service's data directory must be writable by administrators only.
//!
//! The machine journal holds the snapshots the service restores *as SYSTEM*
//! on revert. If a normal user could edit it, they could choose what SYSTEM
//! writes back (within the catalog's targets). A user-owned directory is not
//! enough to fix with a DACL alone — an owner can always rewrite the DACL — so
//! the service takes ownership for Administrators, then applies a protected
//! DACL: SYSTEM and Administrators full control, users read-only (the daemon
//! reads the journal to show status without a round trip).

use std::path::Path;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
};
use windows::core::{BOOL, HSTRING};

/// Owner Administrators; SYSTEM + Administrators full; Users read & execute.
/// `OICI` makes it inherit to files created inside.
const SDDL: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";

/// Lock `dir` and everything directly inside it.
pub fn lock_down(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // Taking ownership of a user-created object needs this privilege, which
    // SYSTEM holds but has disabled.
    let _ = ramwarden_nt::privilege::enable("SeTakeOwnershipPrivilege");
    let _ = ramwarden_nt::privilege::enable("SeRestorePrivilege");

    apply(dir)?;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        apply(&entry.path())?;
    }
    Ok(())
}

fn apply(path: &Path) -> Result<(), String> {
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `sd` is LocalAlloc'd by the call and freed below; the owner and
    // DACL pointers point into it and are used before it is freed.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(SDDL), SDDL_REVISION_1, &mut sd, None)
            .map_err(|e| e.to_string())?;

        let mut owner = PSID::default();
        let mut defaulted = BOOL::default();
        let mut present = BOOL::default();
        let mut dacl = std::ptr::null_mut();
        let r = GetSecurityDescriptorOwner(sd, &mut owner, &mut defaulted)
            .and_then(|()| GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted))
            .map_err(|e| e.to_string())
            .and_then(|()| {
                SetNamedSecurityInfoW(
                    &HSTRING::from(path.as_os_str()),
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    Some(owner),
                    None,
                    Some(dacl),
                    None,
                )
                .ok()
                .map_err(|e| format!("{}: {e}", path.display()))
            });
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        r
    }
}
