//! Lightweight local engine observations: metadata, never full packet-log scans.
use serde::Serialize;
#[cfg(any(target_os = "macos", test))]
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Clone, Debug, Serialize)]
pub struct Observation {
    pub state: &'static str,
    pub detail: String,
    pub source: Option<String>,
    pub observed_at: u64,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct LocalEngines {
    pub clamav: Option<Observation>,
    pub suricata: Option<Observation>,
}
#[cfg(any(target_os = "macos", test))]
fn modified(path: &Path) -> Option<u64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}
#[cfg(any(target_os = "macos", test))]
fn tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(8192)))
        .ok()?;
    let mut text = String::new();
    file.take(8192).read_to_string(&mut text).ok()?;
    Some(text)
}
impl LocalEngines {
    pub fn read() -> Self {
        #[cfg(target_os = "macos")]
        {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            Self::at(
                Path::new("/opt/homebrew/bin"),
                Path::new("/Library/Application Support/ThreatWarden/clamav"),
                Path::new("/var/log/threatwarden"),
                now,
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::default()
        }
    }
    #[cfg(any(target_os = "macos", test))]
    fn at(bin: &Path, signatures: &Path, logs: &Path, now: u64) -> Self {
        let clam_installed = bin.join("clamscan").is_file();
        let main_present = ["main.cvd", "main.cld"]
            .iter()
            .any(|name| signatures.join(name).is_file());
        let daily = ["daily.cvd", "daily.cld"]
            .iter()
            .filter_map(|name| modified(&signatures.join(name)))
            .max();
        let scan_log = logs.join("clamav.json");
        let scan = tail(&scan_log).and_then(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .rfind(|v| v["engine"] == "threatwarden_clamav" && v["event"] == "scan_finished")
        });
        let exit = scan.as_ref().and_then(|s| s["exit_code"].as_i64());
        let clamav = Observation {
            state: if !clam_installed {
                "off"
            } else if daily.is_none() || !main_present {
                "error"
            } else if now.saturating_sub(daily.unwrap_or(0)) > 7 * 86400 {
                "stale"
            } else {
                "ready"
            },
            detail: if !clam_installed {
                "ClamAV scanner not found".into()
            } else {
                format!(
                    "On-demand scanner; signature file updated {}. Last recorded scan exit: {}. No resident scanner is required.",
                    daily
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| "unverified".into()),
                    exit.map(|e| e.to_string())
                        .unwrap_or_else(|| "not recorded".into())
                )
            },
            source: Some(scan_log.display().to_string()),
            observed_at: now,
        };
        let sensor_logs = logs.join("suricata");
        let latest = std::fs::read_dir(&sensor_logs)
            .ok()
            .into_iter()
            .flatten()
            .take(64)
            .filter_map(Result::ok)
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name == "eve.json" || name.starts_with("eve-") && name.ends_with(".json")
            })
            .filter_map(|e| modified(&e.path()).map(|at| (at, e.path())))
            .max_by_key(|(at, _)| *at);
        let fresh = latest
            .as_ref()
            .is_some_and(|(at, _)| now.saturating_sub(*at) <= 120);
        let suricata = Observation {
            state: if !bin.join("suricata").is_file() {
                "off"
            } else if fresh {
                "on"
            } else {
                "unverified"
            },
            detail: if let Some((at, _)) = &latest {
                format!(
                    "Local sensor event log updated {} seconds ago; freshness window is 120 seconds. This is telemetry evidence, not a clean-system verdict.",
                    now.saturating_sub(*at)
                )
            } else {
                "Suricata binary present; no readable event-log metadata found".into()
            },
            source: latest.map(|(_, p)| p.display().to_string()),
            observed_at: now,
        };
        Self {
            clamav: Some(clamav),
            suricata: Some(suricata),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installed_engines_use_current_metadata_and_scan_exit() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let db = dir.path().join("db");
        let logs = dir.path().join("logs");
        for p in [&bin, &db, &logs.join("suricata")] {
            std::fs::create_dir_all(p).unwrap();
        }
        for p in [
            bin.join("clamscan"),
            bin.join("suricata"),
            db.join("daily.cvd"),
            db.join("main.cvd"),
            logs.join("suricata/eve-2026-11-03.json"),
        ] {
            std::fs::write(p, "fixture").unwrap();
        }
        std::fs::write(
            logs.join("clamav.json"),
            "{\"engine\":\"threatwarden_clamav\",\"event\":\"scan_finished\",\"exit_code\":1}\n",
        )
        .unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let report = LocalEngines::at(&bin, &db, &logs, now);
        assert_eq!(report.clamav.unwrap().state, "ready");
        assert_eq!(report.suricata.unwrap().state, "on");
        let stale = LocalEngines::at(&bin, &db, &logs, now + 8 * 86400);
        assert_eq!(stale.clamav.unwrap().state, "stale");
        assert_eq!(stale.suricata.unwrap().state, "unverified");
    }
}
