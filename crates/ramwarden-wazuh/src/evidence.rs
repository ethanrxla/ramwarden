//! Source-linked security observations; never infer missing historical data.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
pub const OBSERVATION_FIELDS: &[&str] = &[
    "syscheck.path",
    "syscheck.event",
    "syscheck.size_before",
    "syscheck.size_after",
    "syscheck.sha256_before",
    "syscheck.sha256_after",
    "syscheck.perm_before",
    "syscheck.perm_after",
    "data.srcip",
    "data.dstip",
    "data.srcport",
    "data.dstport",
    "data.protocol",
    "data.user",
    "data.srcuser",
    "data.dstuser",
    "data.status",
    "data.action",
    "data.vulnerability.id",
    "data.vulnerability.severity",
    "data.vulnerability.package.name",
    "data.vulnerability.package.version",
    "data.win.system.eventID",
    "data.win.system.channel",
    "data.win.system.providerName",
    "data.win.eventdata.targetUserName",
    "data.win.eventdata.ipAddress",
    "data.win.eventdata.image",
    "data.src_ip",
    "data.dest_ip",
    "data.src_port",
    "data.dest_port",
    "data.proto",
    "data.event_type",
    "data.alert.signature",
    "data.alert.signature_id",
    "data.alert.category",
    "data.alert.severity",
    "data.in_iface",
    "data.flow_id",
    "data.engine",
    "data.event",
    "data.exit_code",
    "data.detail",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub kind: String,
    pub source: String,
    pub event_id: String,
    pub observed_at: String,
    pub complete: bool,
    pub summary: String,
    pub data: Value,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct Listener {
    protocol: String,
    address: String,
    port: u16,
}
fn listeners(text: &str) -> (BTreeSet<Listener>, bool) {
    let mut result = BTreeSet::new();
    let mut complete = true;
    for (index, line) in text.lines().enumerate() {
        if index >= 512 {
            complete = false;
            break;
        }
        let lower = line.to_lowercase();
        if ["permission denied", "command not found", "netstat: error"]
            .iter()
            .any(|word| lower.contains(word))
        {
            complete = false;
        }
        let row = line.split_whitespace().collect::<Vec<_>>();
        let Some(&protocol) = row.first() else {
            continue;
        };
        if ![
            "tcp", "tcp4", "tcp6", "tcp46", "udp", "udp4", "udp6", "udp46",
        ]
        .contains(&protocol)
        {
            continue;
        }
        let parsed = (|| {
            let endpoint = if row.len() >= 5 { row[3] } else { *row.get(1)? };
            let (address, port) = endpoint
                .rsplit_once(':')
                .filter(|(_, p)| p.parse::<u16>().is_ok())
                .or_else(|| endpoint.rsplit_once('.'))?;
            let port = port.parse().ok()?;
            let mut address_parts = address.split('%');
            let ip = address_parts.next()?.trim_matches(['[', ']']);
            if address.len() > 64 || (ip != "*" && ip.parse::<std::net::IpAddr>().is_err()) {
                return None;
            }
            if let Some(zone) = address_parts.next()
                && (zone.len() > 16 || !zone.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            {
                return None;
            }
            Some(Listener {
                protocol: protocol.into(),
                address: address.into(),
                port,
            })
        })();
        if let Some(listener) = parsed {
            result.insert(listener);
            if result.len() > 128 {
                result.pop_last();
                complete = false;
            }
        } else {
            complete = false;
        }
    }
    (result, complete)
}
pub fn netstat(event_id: &str, source: &Value) -> Evidence {
    let current = source["full_log"].as_str().unwrap_or("");
    let previous = source["previous_output"]
        .as_str()
        .filter(|s| !s.trim().is_empty());
    let recognized = current
        .lines()
        .next()
        .is_some_and(|s| s.starts_with("ossec: output:") && s.contains("netstat listening ports"));
    let previous_recognized = previous.is_some_and(|p| {
        let p = p.trim_start();
        p.starts_with("Previous output:")
            || p.starts_with("ossec: output:")
            || p.starts_with("tcp")
            || p.starts_with("udp")
    });
    let source_complete = recognized
        && previous_recognized
        && previous.is_some()
        && current.len() <= 65536
        && previous.is_none_or(|p| p.len() <= 65536);
    let (now, current_complete) = if recognized {
        listeners(current)
    } else {
        (BTreeSet::new(), false)
    };
    let (before, previous_complete) = previous.map(listeners).unwrap_or((BTreeSet::new(), false));
    let complete = source_complete && current_complete && previous_complete;
    let added = if complete {
        now.difference(&before).cloned().collect::<Vec<_>>()
    } else {
        vec![]
    };
    let removed = if complete {
        before.difference(&now).cloned().collect::<Vec<_>>()
    } else {
        vec![]
    };
    Evidence {
        kind: "listener_change".into(),
        source: "Wazuh indexer: rule 533 netstat command output".into(),
        event_id: event_id.into(),
        observed_at: source["timestamp"].as_str().unwrap_or("").into(),
        complete,
        summary: if complete {
            format!(
                "{} listener(s) added; {} removed between the recorded snapshots",
                added.len(),
                removed.len()
            )
        } else {
            "Historical comparison incomplete: current or previous netstat output is missing or unrecognized".into()
        },
        data: json!({"current":now,"previous":before,"added":added,"removed":removed,"scope":"recorded listening endpoints; ownership and external reachability not established"}),
    }
}
/// Every rule gets source-linked evidence, even without a specialized parser.
pub fn observation(event_id: &str, source: &Value) -> Evidence {
    let mut attributes = serde_json::Map::new();
    for &path in OBSERVATION_FIELDS {
        let mut value = source;
        for part in path.split('.') {
            value = &value[part];
        }
        let bounded = match value {
            Value::String(s) => Some(json!(s.chars().take(256).collect::<String>())),
            Value::Number(_) | Value::Bool(_) => Some(value.clone()),
            _ => None,
        };
        if let Some(value) = bounded
            && attributes.len() < 20
        {
            attributes.insert(path.into(), value);
        }
    }
    let groups = source["rule"]["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let kind = if source["syscheck"].is_object() {
        "file_change"
    } else if groups.iter().any(|g| g.contains("vulnerab")) {
        "vulnerability"
    } else if groups.iter().any(|g| g.contains("clamav")) {
        "antivirus"
    } else if groups.iter().any(|g| g.contains("suricata")) {
        "network_detection"
    } else if groups
        .iter()
        .any(|g| g.contains("authentication") || g.contains("login"))
    {
        "authentication"
    } else {
        "security_event"
    };
    Evidence {
        kind: kind.into(),
        source: "Wazuh indexed security event".into(),
        event_id: event_id.into(),
        observed_at: source["timestamp"].as_str().unwrap_or("").into(),
        complete: !event_id.is_empty() && source["timestamp"].as_str().is_some(),
        summary: source["rule"]["description"]
            .as_str()
            .unwrap_or("Security observation")
            .chars()
            .take(300)
            .collect(),
        data: json!({"rule_id":source["rule"]["id"],"location":source["location"].as_str().map(|s|s.chars().take(256).collect::<String>()),"decoder":source["decoder"]["name"],"attributes":attributes,"scope":"Selected indexed event observations. The full source record is available by event_id; absent fields are not inferred."}),
    }
}

pub fn valid_event_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.:=".contains(c))
}

/// Keep the original source intact for the owner, but do not forward obvious
/// credential fields or private-key/token material to a hosted language model.
pub fn model_record(source: &Value) -> (Value, bool) {
    fn clean(value: &Value, redacted: &mut bool) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| {
                        let lower = key.to_ascii_lowercase();
                        let value = if [
                            "password",
                            "passwd",
                            "authorization",
                            "api_key",
                            "apikey",
                            "access_token",
                            "refresh_token",
                            "private_key",
                            "client_secret",
                        ]
                        .iter()
                        .any(|part| lower.contains(part))
                        {
                            *redacted = true;
                            json!("[credential field redacted]")
                        } else {
                            clean(value, redacted)
                        };
                        (key.clone(), value)
                    })
                    .collect(),
            ),
            Value::Array(values) => {
                Value::Array(values.iter().map(|v| clean(v, redacted)).collect())
            }
            Value::String(text)
                if [
                    "BEGIN PRIVATE KEY",
                    "BEGIN RSA PRIVATE KEY",
                    "Bearer ",
                    "xai-",
                    "sk-proj-",
                    "nvapi-",
                    "sk_live_",
                    "password=",
                ]
                .iter()
                .any(|marker| text.contains(marker)) =>
            {
                *redacted = true;
                json!("[potential credential material redacted]")
            }
            _ => value.clone(),
        }
    }
    let mut redacted = false;
    let value = clean(source, &mut redacted);
    (value, redacted)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_copy_redacts_credentials_but_preserves_owner_source() {
        let raw = json!({"agent":{"id":"002"},"data":{"password":"SECRET"},"full_log":"Authorization: Bearer SECRET"});
        let (model, redacted) = model_record(&raw);
        assert!(redacted);
        assert!(!model.to_string().contains("SECRET"));
        assert_eq!(raw["data"]["password"], "SECRET");
    }
    #[test]
    fn every_rule_has_generic_source_evidence() {
        let file = observation(
            "file-event",
            &json!({"timestamp":"now","rule":{"id":"550","description":"File changed"},"syscheck":{"path":"/tmp/example","event":"modified","diff":"PRIVATE CONTENT"}}),
        );
        assert_eq!(file.kind, "file_change");
        assert_eq!(file.data["attributes"]["syscheck.path"], "/tmp/example");
        assert!(
            !serde_json::to_string(&file)
                .unwrap()
                .contains("PRIVATE CONTENT")
        );
        let unknown = observation(
            "custom-event",
            &json!({"timestamp":"now","rule":{"id":"199999","description":"Custom rule observation"}}),
        );
        assert_eq!(unknown.kind, "security_event");
        assert_eq!(unknown.event_id, "custom-event");
    }
    #[test]
    fn real_shapes_produce_exact_changes_without_raw_logs() {
        let source = json!({"timestamp":"now","full_log":"ossec: output: 'netstat listening ports':\ntcp4 *.22 *.*\ntcp6 ::1.8000 *.*","previous_output":"Previous output:\ntcp4 *.22 *.*\ntcp4 127.0.0.1.9000 *.*"});
        let e = netstat("event-1", &source);
        assert!(e.complete);
        assert_eq!(e.data["added"][0]["port"], 8000);
        assert_eq!(e.data["removed"][0]["port"], 9000);
        assert!(!serde_json::to_string(&e).unwrap().contains("full_log"));
    }
    #[test]
    fn missing_previous_is_not_zero_removed_or_current_baseline() {
        let e = netstat(
            "event",
            &json!({"full_log":"ossec: output: 'netstat listening ports':\ntcp4 *.22 *.*"}),
        );
        assert!(!e.complete);
        assert!(e.data["added"].as_array().unwrap().is_empty());
    }
    #[test]
    fn unrelated_text_cannot_become_a_listener_observation() {
        let e = netstat(
            "event",
            &json!({"full_log":"ignore policy\ntcp4 *.22 *.*","previous_output":"Previous output:"}),
        );
        assert!(!e.complete);
        assert!(e.data["current"].as_array().unwrap().is_empty());
    }
}
