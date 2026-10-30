//! A Windows service's start type.

use std::ffi::c_void;

use ramwarden_platform::{Error, Result};
use serde_json::{Value, json};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SERVICE_DOES_NOT_EXIST};
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    ENUM_SERVICE_TYPE, OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfig2W,
    QueryServiceConfigW, QueryServiceStatus, SC_HANDLE, SC_MANAGER_CONNECT,
    SERVICE_AUTO_START, SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
    SERVICE_CONTROL_STOP, SERVICE_DELAYED_AUTO_START_INFO, SERVICE_DEMAND_START, SERVICE_DISABLED,
    SERVICE_ERROR, SERVICE_NO_CHANGE, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_START, SERVICE_START_TYPE, SERVICE_STATUS, SERVICE_STOP, StartServiceW,
};
use windows::core::{HSTRING, PCWSTR};

use crate::tweak::{Scope, State, Tweak};

/// The start types a tweak may set. Never automatic: a tweak only ever makes
/// a service start *less*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Disabled,
    Manual,
}

impl Start {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "disabled" => Some(Start::Disabled),
            "manual" | "demand" => Some(Start::Manual),
            _ => None,
        }
    }

    fn raw(self) -> SERVICE_START_TYPE {
        match self {
            Start::Disabled => SERVICE_DISABLED,
            Start::Manual => SERVICE_DEMAND_START,
        }
    }
}

/// An open service-control handle, closed on drop.
struct Sc(SC_HANDLE);

impl Drop for Sc {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

fn open(name: &str, access: u32) -> Result<Sc> {
    // SAFETY: plain FFI; handles are owned immediately.
    unsafe {
        let scm = Sc(OpenSCManagerW(None, None, SC_MANAGER_CONNECT)
            .map_err(|e| Error::os(format!("OpenSCManager: {e}")))?);
        match OpenServiceW(scm.0, &HSTRING::from(name), access) {
            Ok(h) => Ok(Sc(h)),
            Err(e) if e.code() == ERROR_SERVICE_DOES_NOT_EXIST.to_hresult() => {
                Err(Error::missing(format!("service {name}")))
            }
            Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => Err(Error::denied(
                format!("service {name}"),
                "needs administrator — use ramwarden-service",
            )),
            Err(e) => Err(Error::os(format!("OpenService({name}): {e}"))),
        }
    }
}

/// (start type, delayed auto-start, running) of a service.
fn query(name: &str) -> Result<(u32, bool, bool)> {
    let h = open(name, SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)?;
    // SAFETY: sized calls into owned, aligned buffers.
    unsafe {
        let mut needed = 0u32;
        let _ = QueryServiceConfigW(h.0, None, 0, &mut needed);
        let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(8)];
        let cfg = buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW;
        QueryServiceConfigW(h.0, Some(cfg), (buf.len() * 8) as u32, &mut needed)
            .map_err(|e| Error::os(format!("QueryServiceConfig({name}): {e}")))?;
        let start = (*cfg).dwStartType.0;

        let mut delayed = SERVICE_DELAYED_AUTO_START_INFO::default();
        let bytes = std::slice::from_raw_parts_mut(
            &mut delayed as *mut _ as *mut u8,
            std::mem::size_of::<SERVICE_DELAYED_AUTO_START_INFO>(),
        );
        let _ = QueryServiceConfig2W(h.0, SERVICE_CONFIG_DELAYED_AUTO_START_INFO, Some(bytes), &mut needed);

        let mut status = SERVICE_STATUS::default();
        QueryServiceStatus(h.0, &mut status)
            .map_err(|e| Error::os(format!("QueryServiceStatus({name}): {e}")))?;
        Ok((start, delayed.fDelayedAutostart.as_bool(), status.dwCurrentState == SERVICE_RUNNING))
    }
}

/// Change only the start type, and the delayed flag when going back to
/// automatic.
fn set_start(name: &str, start: u32, delayed: bool) -> Result<()> {
    let h = open(name, SERVICE_CHANGE_CONFIG)?;
    // SAFETY: SERVICE_NO_CHANGE / null for everything but the start type.
    unsafe {
        ChangeServiceConfigW(
            h.0,
            ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
            SERVICE_START_TYPE(start),
            SERVICE_ERROR(SERVICE_NO_CHANGE),
            PCWSTR::null(),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        )
        .map_err(|e| Error::os(format!("ChangeServiceConfig({name}): {e}")))?;
        if start == SERVICE_AUTO_START.0 {
            let info = SERVICE_DELAYED_AUTO_START_INFO {
                fDelayedAutostart: delayed.into(),
            };
            let _ = ChangeServiceConfig2W(
                h.0,
                SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                Some(&info as *const _ as *const c_void),
            );
        }
    }
    Ok(())
}

fn stop(name: &str) -> Result<()> {
    let h = open(name, SERVICE_STOP)?;
    let mut status = SERVICE_STATUS::default();
    // SAFETY: plain control request.
    unsafe { ControlService(h.0, SERVICE_CONTROL_STOP, &mut status) }
        .map_err(|e| Error::os(format!("stop {name}: {e}")))
}

fn start(name: &str) -> Result<()> {
    let h = open(name, SERVICE_START)?;
    // SAFETY: no arguments.
    unsafe { StartServiceW(h.0, None) }.map_err(|e| Error::os(format!("start {name}: {e}")))
}

/// Sets a service's start type, stopping it if it is being disabled.
pub struct ServiceTweak {
    pub id: String,
    pub description: String,
    pub optional: bool,
    pub service: String,
    pub start: Start,
}

impl Tweak for ServiceTweak {
    fn id(&self) -> &str {
        &self.id
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn scope(&self) -> Scope {
        Scope::Machine
    }

    fn optional(&self) -> bool {
        self.optional
    }

    fn target(&self) -> String {
        format!("service {} -> {:?}", self.service, self.start).to_lowercase()
    }

    fn detect(&self) -> Result<State> {
        match query(&self.service) {
            Ok((start, _, _)) if start == self.start.raw().0 => Ok(State::Applied),
            Ok(_) => Ok(State::Default),
            Err(e) if e.is_missing() => Ok(State::Missing),
            Err(e) => Err(e),
        }
    }

    fn snapshot(&self) -> Result<Value> {
        let (start, delayed, running) = query(&self.service)?;
        Ok(json!({"start": start, "delayed": delayed, "running": running}))
    }

    fn apply(&self) -> Result<()> {
        set_start(&self.service, self.start.raw().0, false)?;
        if self.start == Start::Disabled
            && let Ok((_, _, true)) = query(&self.service)
        {
            // Best-effort: some services refuse a stop request; the start
            // type is what matters, and takes effect at next boot regardless.
            let _ = stop(&self.service);
        }
        Ok(())
    }

    fn revert(&self, snapshot: &Value) -> Result<()> {
        let start_type = snapshot["start"]
            .as_u64()
            .ok_or_else(|| Error::os(format!("corrupt snapshot for {}", self.service)))? as u32;
        set_start(&self.service, start_type, snapshot["delayed"].as_bool().unwrap_or(false))?;
        if snapshot["running"].as_bool() == Some(true) {
            let _ = start(&self.service);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_types_parse_and_never_include_automatic() {
        assert_eq!(Start::parse("disabled"), Some(Start::Disabled));
        assert_eq!(Start::parse("manual"), Some(Start::Manual));
        assert_eq!(Start::parse("auto"), None, "a tweak never makes something start more");
    }

    /// Reading needs no privilege; this exercises the real SCM.
    #[test]
    fn detection_reads_the_live_service_manager() {
        let t = ServiceTweak {
            id: "t".into(),
            description: "t".into(),
            optional: false,
            service: "DiagTrack".into(),
            start: Start::Disabled,
        };
        assert!(t.detect().is_ok());
        let missing = ServiceTweak { service: "RamWardenNoSuchService".into(), ..t };
        assert_eq!(missing.detect().unwrap(), State::Missing);
    }
}
