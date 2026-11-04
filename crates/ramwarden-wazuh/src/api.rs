//! The Wazuh manager's server API and the Wazuh indexer's search API.
//!
//! Two services, because Wazuh 4.x splits them: the manager (`:55000`) knows
//! each agent's connection state, while alerts are documents in the indexer
//! (`:9200`, index `wazuh-alerts-*`). Either may be left unconfigured; the
//! warden reports what it can.
//!
//! The manager authenticates with a short-lived JWT obtained by basic auth on
//! `POST /security/user/authenticate` (15 minutes by default). The token is
//! cached for less than that and refreshed once on a `401`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use ramwarden_platform::Secret;
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Error, Result};

/// How long a manager token is reused. Wazuh's default lifetime is 900 s.
const TOKEN_REUSE: Duration = Duration::from_secs(600);

/// The most alerts one search returns.
pub const MAX_ALERTS: usize = 50;

/// Where Wazuh is, and how to sign in. Empty URLs mean "not configured".
#[derive(Clone, Debug, Default)]
pub struct Endpoints {
    pub manager_url: String,
    pub api_user: String,
    pub api_password: Secret,
    pub indexer_url: String,
    pub indexer_user: String,
    pub indexer_password: Secret,
    /// PEM root CA for a self-signed install.
    pub ca_pem: Option<Vec<u8>>,
    pub timeout: Duration,
}

/// The manager's view of one agent. Also the shape the hosted API returns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagerAgent {
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// `active`, `disconnected`, `pending` or `never_connected`.
    pub status: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub last_keep_alive: Option<String>,
}

/// One alert, trimmed to what the UI shows. Also the shape the hosted API
/// returns, where the triage fields are filled in by its AI tier.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    #[serde(default)]
    pub id:String,
    #[serde(default,skip_serializing_if="Vec::is_empty")]
    pub evidence_objects:Vec<crate::evidence::Evidence>,
    pub timestamp: String,
    pub rule_id: String,
    pub level: u8,
    pub description: String,
    /// Bounded event metadata only. Never raw logs, diffs or file contents.
    #[serde(default, skip_serializing_if="std::collections::BTreeMap::is_empty")]
    pub evidence: std::collections::BTreeMap<String,String>,
    #[serde(default)]
    pub groups: Vec<String>,
    /// MITRE ATT&CK tactics, when the rule maps to any.
    #[serde(default)]
    pub tactics: Vec<String>,
    /// What happened, in plain language (hosted AI triage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    /// What, if anything, the user should do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advice: Option<String>,
    /// `benign`, `suspicious` or `threat`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// The model's calibrated confidence in `verdict`, 0-1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// A read-only client for one Wazuh deployment.
pub struct Client {
    pub(crate) http: reqwest::Client,
    pub(crate) manager: Option<Url>,
    indexer: Option<Url>,
    ep: Endpoints,
    token: Mutex<Option<(String, Instant)>>,
}

impl Client {
    /// Validate the URLs and build the HTTP client. Refuses any URL that would
    /// send a password in plain text off this machine.
    pub fn new(ep: Endpoints) -> Result<Client> {
        let manager = parse_url(&ep.manager_url)?;
        let indexer = parse_url(&ep.indexer_url)?;
        let mut b = reqwest::Client::builder()
            .timeout(if ep.timeout.is_zero() { Duration::from_secs(10) } else { ep.timeout });
        if let Some(pem) = &ep.ca_pem {
            let certs = reqwest::Certificate::from_pem_bundle(pem)
                .map_err(|e| Error::CaFile("ca_file".into(), e.to_string()))?;
            for cert in certs { b = b.add_root_certificate(cert); }
        }
        let http = b.build().map_err(|e| Error::Unreachable("manager", e.to_string()))?;
        Ok(Client { http, manager, indexer, ep, token: Mutex::new(None) })
    }

    pub fn has_manager(&self) -> bool {
        self.manager.is_some()
    }

    pub fn has_indexer(&self) -> bool {
        self.indexer.is_some()
    }

    /// A manager token, from the cache or a fresh sign-in.
    pub(crate) async fn token(&self, fresh: bool) -> Result<String> {
        if !fresh
            && let Some((t, at)) = self.token.lock().unwrap().as_ref()
            && at.elapsed() < TOKEN_REUSE
        {
            return Ok(t.clone());
        }
        let base = self.manager.as_ref().ok_or(Error::Unreachable("manager", "not configured".into()))?;
        let url = join(base, "security/user/authenticate");
        let resp = self
            .http
            .post(url)
            .basic_auth(&self.ep.api_user, Some(self.ep.api_password.expose()))
            .send()
            .await
            .map_err(|e| Error::Unreachable("manager", e.without_url().to_string()))?;
        match resp.status() {
            s if s.is_success() => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(Error::Auth("manager")),
            s => return Err(Error::Status("manager", s.as_u16())),
        }
        let body: Value = resp.json().await.map_err(|e| Error::Parse("manager", e.to_string()))?;
        let token = body["data"]["token"]
            .as_str()
            .ok_or_else(|| Error::Parse("manager", "no data.token in the sign-in answer".into()))?
            .to_string();
        *self.token.lock().unwrap() = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    /// The manager's view of the agent called `name`, or `None` if it has no
    /// agent by that name (not enrolled yet, or enrolled under another name).
    pub async fn agent(&self, name: &str) -> Result<Option<ManagerAgent>> {
        let base = self.manager.as_ref().ok_or(Error::Unreachable("manager", "not configured".into()))?;
        let mut url = join(base, "agents");
        url.query_pairs_mut()
            .append_pair("name", name)
            .append_pair("select", "id,name,status,version,lastKeepAlive");
        for fresh in [false, true] {
            let token = self.token(fresh).await?;
            let resp = self
                .http
                .get(url.clone())
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| Error::Unreachable("manager", e.without_url().to_string()))?;
            match resp.status() {
                StatusCode::UNAUTHORIZED if !fresh => continue,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(Error::Auth("manager")),
                s if !s.is_success() => return Err(Error::Status("manager", s.as_u16())),
                _ => {}
            }
            let body: Value = resp.json().await.map_err(|e| Error::Parse("manager", e.to_string()))?;
            return Ok(parse_agents(&body)?.into_iter().find(|a| a.name == name));
        }
        Err(Error::Auth("manager"))
    }

    /// Alerts for `agent` at or above `min_level` in the last `hours`, newest
    /// first, at most [`MAX_ALERTS`].
    pub async fn alerts(&self, agent: &str, min_level: u8, hours: u32) -> Result<Vec<Alert>> {
        self.alerts_scoped(agent,None,min_level,hours).await
    }
    pub async fn alerts_for_agent(&self,agent:&str,agent_id:&str,min_level:u8,hours:u32)->Result<Vec<Alert>> {
        self.alerts_scoped(agent,Some(agent_id),min_level,hours).await
    }
    async fn search(&self,query:&Value,limit:usize)->Result<Value>{
        let base=self.indexer.as_ref().ok_or(Error::Unreachable("indexer","not configured".into()))?;
        let mut response=self.http.post(join(base,"wazuh-alerts-*/_search")).basic_auth(&self.ep.indexer_user,Some(self.ep.indexer_password.expose())).json(query).send().await.map_err(|e|Error::Unreachable("indexer",e.without_url().to_string()))?;
        match response.status(){s if s.is_success()=>{},StatusCode::UNAUTHORIZED|StatusCode::FORBIDDEN=>return Err(Error::Auth("indexer")),s=>return Err(Error::Status("indexer",s.as_u16()))}
        let mut bytes=vec![];while let Some(chunk)=response.chunk().await.map_err(|e|Error::Parse("indexer",e.without_url().to_string()))? {if bytes.len()+chunk.len()>limit{return Err(Error::Parse("indexer","response limit exceeded".into()));}bytes.extend_from_slice(&chunk);}
        serde_json::from_slice(&bytes).map_err(|_|Error::Parse("indexer","invalid response".into()))
    }
    async fn alerts_scoped(&self,agent:&str,agent_id:Option<&str>,min_level:u8,hours:u32)->Result<Vec<Alert>> {
        let mut query=alert_query(agent,min_level,hours);
        if let Some(id)=agent_id {query["query"]["bool"]["filter"].as_array_mut().unwrap().push(json!({"term":{"agent.id":id}}));}
        let mut body=self.search(&query,2*1024*1024).await?;
        if let Some(agent_id)=agent_id && let Some(hits)=body["hits"]["hits"].as_array_mut(){hits.retain(|hit|hit["_source"]["agent"]["id"]==agent_id && hit["_source"]["agent"]["name"]==agent);}
        let mut alerts=parse_alerts(&body)?;
        let ids=alerts.iter().filter(|a|a.rule_id=="533" && !a.id.is_empty()).take(8).map(|a|a.id.clone()).collect::<Vec<_>>();
        if !ids.is_empty(){
            query["size"]=json!(ids.len());query["_source"]=json!(["timestamp","full_log","previous_output"]);
            query["query"]["bool"]["filter"].as_array_mut().unwrap().extend([json!({"ids":{"values":ids}}),json!({"term":{"rule.id":"533"}})]);
            if let Ok(body)=self.search(&query,1024*1024).await
                && let Some(hits)=body["hits"]["hits"].as_array(){for hit in hits {
                    if let Some(alert)=alerts.iter_mut().find(|a|hit["_id"].as_str()==Some(&a.id)) {alert.evidence_objects.push(crate::evidence::netstat(&alert.id,&hit["_source"]));}
                }
            }
        }
        Ok(alerts)
    }
    pub async fn events(&self,agent:&str,agent_id:&str,rule:Option<&str>,group:Option<&str>,hours:u32,before:Option<&str>)->Result<Vec<Alert>> {
        let mut query=alert_query(agent,0,hours.clamp(1,720));
        let filters=query["query"]["bool"]["filter"].as_array_mut().unwrap();filters.push(json!({"term":{"agent.id":agent_id}}));
        if let Some(rule)=rule{filters.push(json!({"term":{"rule.id":rule}}));}
        if let Some(group)=group{filters.push(json!({"term":{"rule.groups":group}}));}
        if let Some(before)=before{filters.push(json!({"range":{"timestamp":{"lt":before}}}));}
        {
            let mut body=self.search(&query,2*1024*1024).await?;
            if let Some(hits)=body["hits"]["hits"].as_array_mut(){hits.retain(|hit|hit["_source"]["agent"]["id"]==agent_id && hit["_source"]["agent"]["name"]==agent);}
            parse_alerts(&body)
        }
    }
    pub async fn event(&self,agent:&str,agent_id:&str,id:&str)->Result<Option<Value>> {
        if !crate::evidence::valid_event_id(id){return Err(Error::Parse("event","invalid event id".into()));}
        let query=json!({"size":1,"query":{"bool":{"filter":[{"term":{"agent.name":agent}},{"term":{"agent.id":agent_id}},{"ids":{"values":[id]}}]}}});
        let body=self.search(&query,256*1024).await?;
        Ok(body["hits"]["hits"].as_array().and_then(|hits|hits.first()).filter(|hit|hit["_source"]["agent"]["id"]==agent_id && hit["_source"]["agent"]["name"]==agent).map(|hit|json!({"event_id":hit["_id"],"index":hit["_index"],"source":"Wazuh indexer","read_only":true,"record":hit["_source"]})))
    }

}

/// `""` is "not configured". Anything else must be https, or http to this
/// machine only.
pub(crate) fn parse_url(s: &str) -> Result<Option<Url>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let url = Url::parse(s).map_err(|e| Error::BadUrl(s.to_string(), e.to_string()))?;
    match url.scheme() {
        "https" => Ok(Some(url)),
        "http" if is_loopback(&url) => Ok(Some(url)),
        _ => Err(Error::Insecure(s.to_string())),
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// `base` + `path`, whether or not `base` ends in a slash.
pub(crate) fn join(base: &Url, path: &str) -> Url {
    let mut u = base.clone();
    let joined = format!("{}/{}", base.path().trim_end_matches('/'), path);
    u.set_path(&joined);
    u
}

/// The indexer search for one agent's recent notable alerts.
pub fn alert_query(agent: &str, min_level: u8, hours: u32) -> Value {
    let mut source=vec!["agent.id","agent.name","timestamp","rule.id","rule.level","rule.description","rule.groups","rule.mitre.tactic","location","decoder.name"];
    source.extend_from_slice(crate::evidence::OBSERVATION_FIELDS);
    json!({
        "size": MAX_ALERTS,
        "sort": [{"timestamp": {"order": "desc"}}],
        "_source":source,
        "query": {"bool": {"filter": [
            {"term": {"agent.name": agent}},
            {"range": {"rule.level": {"gte": min_level}}},
            {"range": {"timestamp": {"gte": format!("now-{}h", hours.max(1))}}}
        ]}}
    })
}

/// `GET /agents` → its `data.affected_items`.
pub fn parse_agents(body: &Value) -> Result<Vec<ManagerAgent>> {
    let items = body["data"]["affected_items"]
        .as_array()
        .ok_or_else(|| Error::Parse("manager", "no data.affected_items".into()))?;
    Ok(items
        .iter()
        .filter_map(|a| {
            Some(ManagerAgent {
                id: a["id"].as_str()?.to_string(),
                name: a["name"].as_str()?.to_string(),
                status: a["status"].as_str().unwrap_or("unknown").to_string(),
                version: a["version"].as_str().map(str::to_string),
                last_keep_alive: a["lastKeepAlive"].as_str().map(str::to_string),
            })
        })
        .collect())
}

/// An indexer search answer → its alerts.
pub fn parse_alerts(body: &Value) -> Result<Vec<Alert>> {
    let hits = body["hits"]["hits"]
        .as_array()
        .ok_or_else(|| Error::Parse("indexer", "no hits.hits".into()))?;
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    Ok(hits
        .iter()
        .take(MAX_ALERTS)
        .filter_map(|h| {
            let s = &h["_source"];
            let rule = &s["rule"];
            let event_id=h["_id"].as_str().or_else(||s["id"].as_str()).unwrap_or("").chars().take(256).collect::<String>();
            Some(Alert {
                id:event_id.clone(),
                evidence_objects:vec![crate::evidence::observation(&event_id,s)],
                evidence: [("file", &s["syscheck"]["path"]),("event", &s["syscheck"]["event"]),("source_ip", &s["data"]["srcip"]),("destination_ip", &s["data"]["dstip"]),("process", &s["data"]["win"]["eventdata"]["image"])]
                    .into_iter().filter_map(|(k,v)|v.as_str().map(|v|(k.to_string(),v.chars().take(512).collect()))).collect(),
                timestamp: s["timestamp"].as_str().unwrap_or_default().to_string(),
                rule_id: match &rule["id"] {
                    Value::String(id) => id.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => String::new(),
                },
                level: u8::try_from(rule["level"].as_u64()?).unwrap_or(u8::MAX),
                description: rule["description"].as_str().unwrap_or_default().to_string(),
                groups: strings(&rule["groups"]),
                tactics: strings(&rule["mitre"]["tactic"]),
                explanation: None,
                advice: None,
                verdict: None,
                confidence: None,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_evidence_excludes_raw_logs_and_file_contents() {
        let alerts=parse_alerts(&json!({"hits":{"hits":[{"_source":{"timestamp":"now","rule":{"id":"550","level":7,"description":"File changed"},"syscheck":{"path":"/tmp/example","event":"modified","diff":"SECRET CONTENT"},"full_log":"SECRET RAW LOG"}}]}})).unwrap();
        assert_eq!(alerts[0].evidence["file"],"/tmp/example");
        let serialized=serde_json::to_string(&alerts).unwrap();assert!(!serialized.contains("SECRET"));
        let query=alert_query("agent",7,24);assert!(!query["_source"].as_array().unwrap().iter().any(|s|s=="full_log" || s=="syscheck.diff"));
    }

    #[test]
    fn plain_http_is_refused_off_this_machine() {
        assert!(matches!(parse_url("http://wazuh.lan:55000"), Err(Error::Insecure(_))));
        assert!(matches!(parse_url("http://10.0.0.5:9200"), Err(Error::Insecure(_))));
        assert!(matches!(parse_url("ftp://wazuh.lan"), Err(Error::Insecure(_))));
        assert!(parse_url("https://wazuh.lan:55000").unwrap().is_some());
        assert!(parse_url("http://127.0.0.1:55000").unwrap().is_some());
        assert!(parse_url("http://localhost:9200").unwrap().is_some());
        assert!(parse_url("http://[::1]:9200").unwrap().is_some());
        assert!(parse_url("  ").unwrap().is_none());
        assert!(matches!(parse_url("not a url"), Err(Error::BadUrl(..))));
    }

    #[test]
    fn joins_paths_with_or_without_a_trailing_slash() {
        let a = Url::parse("https://w:55000").unwrap();
        let b = Url::parse("https://w:55000/api/").unwrap();
        assert_eq!(join(&a, "agents").as_str(), "https://w:55000/agents");
        assert_eq!(join(&b, "agents").as_str(), "https://w:55000/api/agents");
    }

    #[test]
    fn parses_the_managers_agent_list() {
        let body = json!({"data": {"affected_items": [
            {"id": "003", "name": "ryzen-desk", "status": "active", "version": "Wazuh v4.14.0",
             "lastKeepAlive": "2026-11-01T08:00:00Z"},
            {"name": "no-id"}
        ], "total_affected_items": 2}, "error": 0});
        let a = parse_agents(&body).unwrap();
        assert_eq!(a.len(), 1, "an item without an id is skipped");
        assert_eq!(a[0].id, "003");
        assert_eq!(a[0].status, "active");
        assert_eq!(a[0].version.as_deref(), Some("Wazuh v4.14.0"));
        assert!(parse_agents(&json!({"error": 1})).is_err());
    }

    #[test]
    fn parses_indexer_alerts() {
        let body = json!({"hits": {"hits": [
            {"_source": {"timestamp": "2026-11-01T07:59:00.000+0000",
              "rule": {"id": "5712", "level": 10, "description": "sshd: brute force",
                       "groups": ["syslog", "sshd"], "mitre": {"tactic": ["Credential Access"]}}}},
            {"_source": {"timestamp": "t", "rule": {"id": 550, "level": 7, "description": "Integrity checksum changed."}}},
            {"_source": {"rule": {"id": "1"}}}
        ]}});
        let a = parse_alerts(&body).unwrap();
        assert_eq!(a.len(), 2, "an alert without a level is skipped");
        assert_eq!(a[0].level, 10);
        assert_eq!(a[0].tactics, vec!["Credential Access"]);
        assert_eq!(a[1].rule_id, "550", "numeric rule ids are accepted");
        assert!(a[1].groups.is_empty());
    }

    #[test]
    fn the_alert_query_filters_by_agent_level_and_window() {
        let q = alert_query("ryzen-desk", 7, 24);
        let f = &q["query"]["bool"]["filter"];
        assert_eq!(f[0]["term"]["agent.name"], "ryzen-desk");
        assert_eq!(f[1]["range"]["rule.level"]["gte"], 7);
        assert_eq!(f[2]["range"]["timestamp"]["gte"], "now-24h");
        assert_eq!(q["size"], MAX_ALERTS);
    }
}
