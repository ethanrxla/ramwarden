//! Desktop features that exist on one operating system only.
//!
//! Terminal sessions and private-browsing launchers are found by walking
//! `/proc`, and the page-out helper is a Unix socket. None of that has a
//! platform-neutral shape worth inventing yet, so the routes call through here:
//! on Linux these delegate to `ramwarden-core`, elsewhere they report nothing,
//! and the JSON the routes emit keeps the same shape on both.

use serde_json::{Value, json};

#[cfg(target_os = "linux")]
mod imp {
    use ramwarden_core::{private, terminals};
    use ramwarden_kernel::Root;
    use serde_json::{Value, json};

    pub use ramwarden_core::terminals::Terminal;

    pub fn terminals() -> Vec<Terminal> {
        terminals::list(&Root::system()).unwrap_or_default()
    }

    pub fn terminal_summary(t: &[Terminal]) -> String {
        terminals::summary(t)
    }

    pub fn close_idle_terminals(t: &[Terminal]) -> Vec<i32> {
        terminals::close_idle(t)
    }

    pub fn private_windows() -> Vec<Value> {
        private::detect(&Root::system(), false)
            .iter()
            .map(|c| {
                json!({
                    "type": c.kind.as_str(), "title": c.title,
                    "rss_mb": crate::routes::mb(c.pss), "closeable": c.closeable,
                })
            })
            .collect()
    }

    pub fn helper() -> Value {
        json!({
            "helper_socket": ramwarden_kernel::helper::socket_path(),
            "helper_present": ramwarden_kernel::helper::present(),
        })
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use serde_json::{Value, json};

    /// The shape of a Linux terminal session, so the routes compile unchanged.
    /// Never constructed off Linux.
    pub struct Terminal {
        pub pid: i32,
        pub shell: String,
        pub is_idle: bool,
        pub children: Vec<String>,
    }

    impl Terminal {
        pub fn reason(&self) -> String {
            String::new()
        }
    }

    pub fn terminals() -> Vec<Terminal> {
        Vec::new()
    }

    pub fn terminal_summary(_: &[Terminal]) -> String {
        "terminal tracking is Linux-only".into()
    }

    pub fn close_idle_terminals(_: &[Terminal]) -> Vec<i32> {
        Vec::new()
    }

    pub fn private_windows() -> Vec<Value> {
        Vec::new()
    }

    pub fn helper() -> Value {
        json!({"helper_present": false})
    }
}

pub use imp::*;

/// Terminals as the `/stats` route reports them.
pub fn terminals_json(terms: &[Terminal]) -> Value {
    json!({
        "idle": terms.iter().filter(|t| t.is_idle)
            .map(|t| json!({"pid": t.pid, "shell": t.shell})).collect::<Vec<_>>(),
        "busy": terms.iter().filter(|t| !t.is_idle)
            .map(|t| json!({"pid": t.pid, "shell": t.shell,
                            "children": t.children.iter().take(5).collect::<Vec<_>>()}))
            .collect::<Vec<_>>(),
    })
}
