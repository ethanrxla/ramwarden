//! The Connected devices pane's view model, shared by every client.
//!
//! The daemon's `GET /devices` reports this computer's hardware and the
//! user's Tailscale devices. This module turns that into the lines each
//! client draws, so the Windows window, the GTK window and the Mac menu say
//! the same thing. Wording changes belong here.

use serde::Deserialize;

/// One device on the user's Tailscale network.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub this_device: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ThisComputer {
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub cpu_model: Option<String>,
    #[serde(default)]
    pub physical_cores: Option<u32>,
    #[serde(default)]
    pub logical_cores: Option<u32>,
    #[serde(default)]
    pub ram_total_bytes: u64,
}

/// From `/devices`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct DevicesPane {
    #[serde(default)]
    pub this_computer: ThisComputer,
    /// Whether Tailscale answered. When false, `reason` says why.
    #[serde(default)]
    pub tailscale: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub devices: Vec<Device>,
    /// Whether the warden may ask other devices hardware questions over SSH.
    #[serde(default)]
    pub ssh_checks: bool,
}

impl ThisComputer {
    /// "AMD Ryzen 7 9700X · 8 cores (16 threads) · 31.1 GB RAM".
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(m) = &self.cpu_model {
            parts.push(m.clone());
        }
        match (self.physical_cores, self.logical_cores) {
            (Some(p), Some(l)) if p != l => parts.push(format!("{p} cores ({l} threads)")),
            (Some(p), _) => parts.push(format!("{p} cores")),
            (None, Some(l)) => parts.push(format!("{l} logical processors")),
            (None, None) => {}
        }
        if self.ram_total_bytes > 0 {
            parts.push(format!("{:.1} GB RAM", self.ram_total_bytes as f64 / 1_073_741_824.0));
        }
        if parts.is_empty() { "Hardware not reported".into() } else { parts.join(" · ") }
    }
}

impl Device {
    /// "online", "offline" or "this computer".
    pub fn status(&self) -> &'static str {
        if self.this_device {
            "this computer"
        } else if self.online {
            "online"
        } else {
            "offline"
        }
    }

    /// Tailscale's OS name, as people write it.
    pub fn os_label(&self) -> &str {
        match self.os.as_str() {
            "linux" => "Linux",
            "windows" => "Windows",
            "macOS" | "darwin" => "macOS",
            "iOS" => "iOS",
            "android" => "Android",
            "" => "Unknown",
            other => other,
        }
    }
}

impl DevicesPane {
    /// The line under the device list: what the warden can and cannot do.
    pub fn footnote(&self) -> String {
        if !self.tailscale {
            return self.reason.clone().unwrap_or_else(|| "Tailscale is not available.".into());
        }
        if self.ssh_checks {
            "Ask the warden about any online computer here, for example \"how many CPU cores does my laptop have?\". It runs one read-only hardware command over SSH with your own keys.".into()
        } else {
            "The warden can list these devices. To let it answer hardware questions about them, set devices_ssh = true under [assistant] in ramwarden.toml.".into()
        }
    }

    /// Other devices, this computer left out (it has its own line).
    pub fn others(&self) -> impl Iterator<Item = &Device> {
        self.devices.iter().filter(|d| !d.this_device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_daemon_report_reads_into_lines() {
        let pane: DevicesPane = serde_json::from_str(
            r#"{"this_computer":{"os":"windows","cpu_model":"AMD Ryzen 7 9700X 8-Core Processor","physical_cores":8,"logical_cores":16,"ram_total_bytes":33393000000},
                "tailscale":true,"ssh_checks":false,
                "devices":[{"name":"RXLA_PC_1","os":"windows","online":true,"this_device":true},
                           {"name":"Ethan-Legion5","os":"linux","address":"100.64.0.2","online":true,"this_device":false},
                           {"name":"pixel","os":"android","online":false,"this_device":false}]}"#,
        )
        .unwrap();
        assert_eq!(pane.this_computer.summary(), "AMD Ryzen 7 9700X 8-Core Processor · 8 cores (16 threads) · 31.1 GB RAM");
        let others: Vec<_> = pane.others().map(|d| (d.name.as_str(), d.os_label(), d.status())).collect();
        assert_eq!(others, [("Ethan-Legion5", "Linux", "online"), ("pixel", "Android", "offline")]);
        assert!(pane.footnote().contains("devices_ssh = true"));
    }

    #[test]
    fn no_tailscale_says_why() {
        let pane: DevicesPane = serde_json::from_str(r#"{"tailscale":false,"reason":"Tailscale is not installed on this computer, so RamWarden cannot see your other devices."}"#).unwrap();
        assert!(pane.footnote().starts_with("Tailscale is not installed"));
        assert_eq!(pane.this_computer.summary(), "Hardware not reported");
    }
}
