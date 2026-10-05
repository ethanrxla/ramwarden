//! What the service does with each request — and what it refuses.

use ramwarden_core::roles::classify;
use ramwarden_debloat::{Debloat, Scope};
use ramwarden_nt::pipe::{Request, Response};
use ramwarden_nt::{snapshot, standby, workingset};
use serde_json::json;

/// Executes requests on behalf of one connected client.
pub struct Handler {
    /// The session the client runs in, from its pipe credentials. A client
    /// may only ever affect processes in its own session.
    client_session: Option<u32>,
}

impl Handler {
    pub fn new(client_pid: Option<u32>) -> Self {
        let client_session = client_pid.and_then(|pid| {
            snapshot::processes()
                .ok()?
                .into_iter()
                .find(|p| p.pid == pid)
                .map(|p| p.session)
        });
        Handler { client_session }
    }

    pub fn handle(&self, req: Request) -> Response {
        match req {
            Request::Ping => Response::Ok {
                bytes: 0,
                detail: json!({"version": env!("CARGO_PKG_VERSION")}),
            },
            Request::PurgeLowPriorityStandby => match standby::purge_low_priority() {
                Ok(()) => Response::ok(),
                Err(e) => Response::error(e.to_string()),
            },
            Request::Trim { pid } => self.trim(pid),
            Request::TweakApply { id } => machine_tweak(&id, |d, t| {
                d.apply(t).map(|r| serde_json::to_value(r).unwrap_or_default())
            }),
            Request::TweakRevert { id } => machine_tweak(&id, |d, t| {
                d.revert(t).map(|r| serde_json::to_value(r).unwrap_or_default())
            }),
            Request::TweakStatus => match Debloat::for_service().and_then(|d| {
                Ok(d.status_all()?
                    .into_iter()
                    .filter(|(_, s)| s.scope == Scope::Machine)
                    .map(|(_, s)| s)
                    .collect::<Vec<_>>())
            }) {
                Ok(all) => Response::Ok {
                    bytes: 0,
                    detail: json!(all),
                },
                Err(e) => Response::error(e.to_string()),
            },
        }
    }

    /// Trim a process the user cannot open themselves — an elevated app in
    /// their own session. Everything that could be structurally protected, or
    /// that belongs to another session, is refused.
    fn trim(&self, pid: u32) -> Response {
        if pid <= 4 {
            return Response::error(format!("pid {pid} is the kernel"));
        }
        let Ok(table) = snapshot::processes() else {
            return Response::error("cannot read the process table");
        };
        let Some(p) = table.iter().find(|p| p.pid == pid) else {
            return Response::error(format!("pid {pid} is not running"));
        };
        if p.session == 0 {
            return Response::error(format!("{} (pid {pid}) is a service", p.name));
        }
        if self.client_session != Some(p.session) {
            return Response::error(format!("{} (pid {pid}) is not in your session", p.name));
        }
        let role = classify(&p.name, "", None);
        if role.is_structural() {
            return Response::error(format!("{} (pid {pid}): {}", p.name, role.protection_reason()));
        }
        match workingset::trim(pid) {
            Ok(bytes) => Response::Ok {
                bytes,
                detail: json!(null),
            },
            Err(e) => Response::error(e.to_string()),
        }
    }
}

/// Look `id` up in the shipped catalog and run `f` on it, machine scope only.
fn machine_tweak(
    id: &str,
    f: impl FnOnce(&Debloat, &dyn ramwarden_debloat::Tweak) -> ramwarden_platform::Result<serde_json::Value>,
) -> Response {
    let tweak = match ramwarden_debloat::catalog::tweak(id) {
        Ok(t) => t,
        Err(e) => return Response::error(e.to_string()),
    };
    if tweak.scope() != Scope::Machine {
        return Response::error(format!("{id} is a per-user tweak; apply it as the user"));
    }
    match Debloat::for_service().and_then(|d| f(&d, tweak.as_ref())) {
        Ok(detail) => {
            tracing::info!("tweak {id}: {detail}");
            Response::Ok { bytes: 0, detail }
        }
        Err(e) => Response::error(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(r: Response) -> String {
        match r {
            Response::Error { message } => message,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_kernel_and_services_are_refused() {
        let h = Handler::new(Some(std::process::id()));
        assert!(err(h.handle(Request::Trim { pid: 4 })).contains("kernel"));
        let svc = snapshot::processes().unwrap().into_iter().find(|p| p.session == 0 && p.pid > 4).unwrap();
        assert!(err(h.handle(Request::Trim { pid: svc.pid })).contains("service"));
    }

    #[test]
    fn structural_processes_are_refused_even_in_the_users_session() {
        let h = Handler::new(Some(std::process::id()));
        let explorer = snapshot::processes().unwrap().into_iter().find(|p| p.name.eq_ignore_ascii_case("explorer"));
        if let Some(e) = explorer {
            assert!(err(h.handle(Request::Trim { pid: e.pid })).contains("compositor"));
        }
    }

    #[test]
    fn an_unknown_client_session_can_trim_nothing() {
        let h = Handler::new(None);
        let me = std::process::id();
        assert!(err(h.handle(Request::Trim { pid: me })).contains("not in your session"));
    }

    #[test]
    fn tweaks_outside_the_catalog_or_in_user_scope_are_refused() {
        let h = Handler::new(None);
        assert!(err(h.handle(Request::TweakApply { id: "privacy.disable-defender".into() })).contains("not in any shipped profile"));
        assert!(err(h.handle(Request::TweakApply { id: "privacy.copilot".into() })).contains("per-user"));
    }
}
