//! Server-only enrollment. Does not expose active response or arbitrary manager operations.
use crate::{Client, Endpoints, Error, Result};
use reqwest::Method;
use serde_json::{Value, json};
pub struct EnrollmentClient {
    client: Client,
}
impl EnrollmentClient {
    pub fn new(ep: Endpoints) -> Result<Self> {
        Ok(Self {
            client: Client::new(ep)?,
        })
    }
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let base = self
            .client
            .manager
            .as_ref()
            .ok_or(Error::Unreachable("manager", "not configured".into()))?;
        let (path, query) = path
            .split_once('?')
            .map_or((path, None), |(p, q)| (p, Some(q)));
        let mut url = crate::api::join(base, path);
        url.set_query(query);
        let mut req = self
            .client
            .http
            .request(method, url)
            .bearer_auth(self.client.token(false).await?);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let response = req
            .send()
            .await
            .map_err(|e| Error::Unreachable("manager", e.without_url().to_string()))?;
        if !response.status().is_success() {
            return Err(Error::Status("manager", response.status().as_u16()));
        }
        let value: Value = response
            .json()
            .await
            .map_err(|_| Error::Parse("manager", "invalid enrollment response".into()))?;
        if value["data"]["failed_items"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            return Err(Error::Parse(
                "manager",
                "enrollment operation failed".into(),
            ));
        }
        Ok(value)
    }
    /// Returns the manager id and only this deterministic agent's enrollment key.
    pub async fn enroll(&self, name: &str, group: &str) -> Result<(String, String)> {
        if !name.starts_with("rw-")
            || !group.starts_with("acct-")
            || [name, group].iter().any(|s| {
                s.len() > 100 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err(Error::Parse(
                "manager",
                "invalid server-assigned agent identity".into(),
            ));
        }
        let groups = self
            .call(Method::GET, &format!("groups?groups_list={group}"), None)
            .await;
        let exists = groups
            .as_ref()
            .ok()
            .and_then(|v| v["data"]["affected_items"].as_array())
            .is_some_and(|a| a.iter().any(|v| v["name"] == group));
        if !exists
            && self
                .call(Method::POST, "groups", Some(json!({"group_id":group})))
                .await
                .is_err()
        {
            let groups = self
                .call(Method::GET, &format!("groups?groups_list={group}"), None)
                .await?;
            if !groups["data"]["affected_items"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v["name"] == group))
            {
                return Err(Error::Parse("manager", "account group unavailable".into()));
            }
        }
        let agent = match self.client.agent(name).await? {
            Some(agent) => agent,
            None => {
                let _ = self
                    .call(
                        Method::POST,
                        "agents",
                        Some(json!({"name":name,"ip":"any"})),
                    )
                    .await;
                self.client.agent(name).await?.ok_or(Error::Parse(
                    "manager",
                    "agent registration incomplete; retry safely".into(),
                ))?
            }
        };
        if agent.id.is_empty() || !agent.id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::Parse("manager", "invalid agent id".into()));
        }
        self.call(
            Method::PUT,
            &format!("agents/{}/group/{group}?force_single_group=false", agent.id),
            None,
        )
        .await?;
        let value = self
            .call(Method::GET, &format!("agents/{}/key", agent.id), None)
            .await?;
        let key = value["data"]["affected_items"][0]["key"]
            .as_str()
            .ok_or(Error::Parse("manager", "enrollment key unavailable".into()))?;
        Ok((agent.id, key.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::Query,
        routing::{get, post, put},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[tokio::test]
    async fn enrollment_retry_reuses_the_agent_and_keeps_the_baseline_group() {
        let created = Arc::new(AtomicUsize::new(0));
        let read = created.clone();
        let write = created.clone();
        let app=Router::new()
            .route("/security/user/authenticate",post(||async{Json(json!({"data":{"token":"test"}}))}))
            .route("/groups",get(||async{Json(json!({"data":{"affected_items":[{"name":"acct-test"}]}}))}))
            .route("/agents",get(move ||{let n=read.load(Ordering::SeqCst);async move{Json(json!({"data":{"affected_items":if n>0{vec![json!({"id":"042","name":"rw-test","status":"active"})]}else{vec![]}}}))}})
                .post(move |Json(body):Json<Value>|{let write=write.clone();async move{assert_eq!(body["name"],"rw-test");write.fetch_add(1,Ordering::SeqCst);Json(json!({"data":{"affected_items":[{"id":"042"}]}}))}}))
            .route("/agents/042/group/acct-test",put(|Query(q):Query<std::collections::HashMap<String,String>>|async move{assert_eq!(q["force_single_group"],"false");Json(json!({"data":{"affected_items":["042"],"failed_items":[]}}))}))
            .route("/agents/042/key",get(||async{Json(json!({"data":{"affected_items":[{"key":"only-this-agent-key"}]}}))}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = EnrollmentClient::new(Endpoints {
            manager_url: url,
            ..Default::default()
        })
        .unwrap();
        for _ in 0..2 {
            assert_eq!(
                client.enroll("rw-test", "acct-test").await.unwrap(),
                ("042".into(), "only-this-agent-key".into())
            );
        }
        assert_eq!(created.load(Ordering::SeqCst), 1);
        assert!(client.enroll("../../foreign", "acct-test").await.is_err());
        server.abort();
    }
}
