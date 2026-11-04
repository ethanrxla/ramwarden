//! Endpoint evidence, not an assumption that an installed service is healthy.
//! File reads are bounded; missing, unreadable or stale evidence stays unknown.
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

pub mod network;
pub use network::Network;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Sensors {
    pub checked_at: u64,
    pub clamav: Sensor,
    pub suricata: Sensor,
    pub network: Network,
}

#[derive(Clone, Debug, Serialize)]
pub struct Sensor {
    pub state: &'static str,
    pub detail: String,
    pub evidence_at: Option<u64>,
}

impl Default for Sensor {
    fn default() -> Self {
        Self::unknown("Not checked")
    }
}

impl Sensor {
    fn unknown(detail: impl Into<String>) -> Self {
        Self {
            state: "unknown",
            detail: detail.into(),
            evidence_at: None,
        }
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub async fn collect() -> Sensors {
    // Disk reads run away from the async executor. Connection commands have a deadline.
    let files = tokio::task::spawn_blocking(|| {
        let (scan, db, eve) = paths();
        (clamav(&scan, &db, now()), suricata(&eve, now()))
    });
    let network = network::collect().await;
    let (clamav, suricata) = files.await.unwrap_or_default();
    Sensors {
        checked_at: now(),
        clamav,
        suricata,
        network,
    }
}

fn paths() -> (PathBuf, PathBuf, PathBuf) {
    if cfg!(windows) {
        let base = PathBuf::from(
            std::env::var_os("ProgramData").unwrap_or_else(|| "C:/ProgramData".into()),
        );
        let official = PathBuf::from(
            std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:/Program Files".into()),
        )
        .join("ClamAV/database");
        (
            base.join("ThreatWarden/logs/clamav.json"),
            if official.is_dir() {
                official
            } else {
                "D:/ThreatWarden/ClamAV/database".into()
            },
            "D:/ThreatWarden/Suricata/log".into(),
        )
    } else if cfg!(target_os = "macos") {
        (
            "/var/log/threatwarden/clamav.json".into(),
            "/Library/Application Support/ThreatWarden/clamav".into(),
            "/var/log/threatwarden/suricata".into(),
        )
    } else {
        (
            "/var/log/threatwarden/clamav.json".into(),
            "/var/lib/clamav".into(),
            "/var/log/suricata".into(),
        )
    }
}

fn modified(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Only complete JSON lines; the initial truncated line and a writer's pending
/// final line are discarded. Rotation is handled by opening the latest file anew.
fn tail(path: &Path) -> std::io::Result<Vec<Value>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(512 * 1024);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.take(512 * 1024).read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<_> = text.split('\n').collect();
    lines.pop();
    Ok(lines
        .into_iter()
        .skip(usize::from(start > 0))
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect())
}

fn fresh(time: u64, now: u64, age: u64) -> bool {
    time <= now.saturating_add(5) && now.saturating_sub(time) <= age
}

fn clamav(log: &Path, db: &Path, now: u64) -> Sensor {
    let signatures = ["daily.cvd", "daily.cld"]
        .iter()
        .filter_map(|f| modified(&db.join(f)))
        .max();
    let Some(sig) = signatures else {
        return Sensor::unknown("Signatures missing or unreadable; scans unverified");
    };
    if !fresh(sig, now, 3 * 86400) {
        return Sensor {
            state: "error",
            detail: "Signatures are over three days old; refresh required".into(),
            evidence_at: Some(sig),
        };
    }
    let Ok(events) = tail(log) else {
        return Sensor::unknown("Signatures present; scan history unreadable");
    };
    let Some(event) = events
        .iter()
        .rev()
        .find(|v| v["engine"] == "threatwarden_clamav" && v["event"] != "malware_found")
    else {
        return Sensor::unknown("Signatures present; no scan recorded");
    };
    let timestamp = event["time"].as_u64();
    let age = timestamp.map(|t| now.saturating_sub(t) / 3600);
    let (state, detail) = match event["event"].as_str() {
        Some("scan_deferred") => (
            "watch",
            "Scheduled scan deferred: memory not clear".to_string(),
        ),
        Some("scan_started") if timestamp.is_some_and(|t| fresh(t, now, 4 * 3600)) => {
            ("watch", "Scan started; completion not yet confirmed".to_string())
        }
        Some("scan_finished") if event["exit_code"] == 1 => (
            "watch",
            "Last scan found a detection; review alerts".to_string(),
        ),
        Some("scan_finished") if event["exit_code"] == 0 && timestamp.is_some_and(|t| fresh(t, now, 48 * 3600)) => (
            "on",
            format!(
                "Last scan finished {}h ago; selected folders only",
                age.unwrap_or(0)
            ),
        ),
        Some("scan_finished") if event["exit_code"] == 0 => (
            "watch",
            "No timestamped successful scan in the last 48 hours".to_string(),
        ),
        _ => (
            "error",
            "Last scan failed or its completion is unverified".to_string(),
        ),
    };
    Sensor {
        state,
        detail,
        evidence_at: timestamp,
    }
}

fn suricata(directory: &Path, now: u64) -> Sensor {
    let latest = fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("eve") && s.ends_with(".json"))
        })
        .filter_map(|p| modified(&p).map(|m| (m, p)))
        .max_by_key(|(m, _)| *m);
    let Some((mtime, path)) = latest else {
        return Sensor::unknown("No readable EVE capture evidence");
    };
    if !fresh(mtime, now, 120) {
        return Sensor {
            state: "error",
            detail: "Capture evidence is stale; protection unverified".into(),
            evidence_at: Some(mtime),
        };
    }
    let Ok(events) = tail(&path) else {
        return Sensor::unknown("Capture log cannot be read");
    };
    // Stats continue even on a quiet network. Flow/alert traffic alone does not
    // prove that a dead capture thread is still monitoring the adapter.
    let Some(stats) = events.iter().rev().find(|v| v["event_type"] == "stats") else {
        return Sensor::unknown("EVE activity present; capture counters unavailable");
    };
    let stats_time = stats["timestamp"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .and_then(|t| u64::try_from(t.timestamp()).ok());
    if !stats_time.is_some_and(|t| fresh(t, now, 120)) {
        return Sensor::unknown("Capture counters are stale or have no valid timestamp");
    }
    let packets = stats
        .pointer("/stats/decoder/pkts")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let drops = stats
        .pointer("/stats/capture/kernel_drops")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Sensor {
        state: if packets > 0 { "on" } else { "watch" },
        detail: format!(
            "Passive IDS: {packets} packets, {drops} capture drops since start; encrypted payloads remain encrypted"
        ),
        evidence_at: Some(mtime),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_evidence_never_means_protected() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            clamav(&d.path().join("missing"), d.path(), now()).state,
            "unknown"
        );
        assert_eq!(suricata(d.path(), now()).state, "unknown");
    }
    #[test]
    fn rotation_partial_records_and_stale_capture() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("eve-old.json"), "{\"event_type\":\"flow\"}\n").unwrap();
        let time = chrono::DateTime::from_timestamp(now() as i64, 0)
            .unwrap()
            .to_rfc3339();
        fs::write(d.path().join("eve-new.json"), format!("{{\"event_type\":\"stats\",\"timestamp\":\"{time}\",\"stats\":{{\"decoder\":{{\"pkts\":42}}}}}}\n{{partial")).unwrap();
        // Remove the old file to avoid filesystem timestamp ties.
        fs::remove_file(d.path().join("eve-old.json")).unwrap();
        assert_eq!(suricata(d.path(), now()).state, "on");
        assert_eq!(suricata(d.path(), now() + 121).state, "error");
    }
    #[test]
    fn deferral_failure_and_legacy_scan_are_not_success() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("clamav.json");
        fs::write(d.path().join("daily.cvd"), "fixture").unwrap();
        for (event, code, expected) in [
            ("scan_deferred", 0, "watch"),
            ("scan_finished", 2, "error"),
            ("scan_finished", 1, "watch"),
            ("scan_finished", 0, "on"),
        ] {
            fs::write(&log, format!("{{\"engine\":\"threatwarden_clamav\",\"event\":\"{event}\",\"exit_code\":{code},\"time\":{}}}\n", now())).unwrap();
            assert_eq!(clamav(&log, d.path(), now()).state, expected);
        }
        fs::write(
            &log,
            "{\"engine\":\"threatwarden_clamav\",\"event\":\"scan_finished\",\"exit_code\":0}\n",
        )
        .unwrap();
        assert_eq!(clamav(&log, d.path(), now()).state, "watch");
        fs::write(&log, format!("{{\"engine\":\"threatwarden_clamav\",\"event\":\"scan_finished\",\"exit_code\":0,\"time\":{}}}\n", now() + 3600)).unwrap();
        assert_eq!(clamav(&log, d.path(), now()).state, "watch");
        assert_eq!(clamav(&log, d.path(), now() + 4 * 86400).state, "error");
    }
    #[test]
    fn fresh_file_does_not_make_old_stats_current() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("eve.json"), "{\"timestamp\":\"2020-01-01T00:00:00Z\",\"event_type\":\"stats\",\"stats\":{\"decoder\":{\"pkts\":42}}}\n{\"event_type\":\"flow\"}\n").unwrap();
        assert_eq!(suricata(d.path(), now()).state, "unknown");
    }
}
