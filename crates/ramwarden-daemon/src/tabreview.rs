//! Local, one-shot review of uncertain tab closures. No hosted data storage.
use axum::{Json, Router, extract::State, http::StatusCode, routing::{get,post}};
use serde::{Deserialize,Serialize};
use crate::{browser::{Row,Target},hub::AppState};

#[derive(Clone,Serialize)]
pub struct Proposal { pub token:String, pub expires_at:u64, pub tabs:Vec<Row> }
#[derive(Default)]
pub struct ReviewState { pending:Option<Proposal>, last_prompt:u64 }
impl ReviewState {
    fn offer(&mut self,rows:Vec<Row>,now:u64)->Option<Proposal> {
        if let Some(p)=&self.pending && now<p.expires_at { return Some(p.clone()); }
        self.pending=None;
        if rows.is_empty() || now.saturating_sub(self.last_prompt)<1800 {return None;}
        let p=Proposal{token:uuid::Uuid::new_v4().to_string(),expires_at:now+600,tabs:rows};
        self.last_prompt=now; self.pending=Some(p.clone()); Some(p)
    }
    fn consume(&mut self,token:&str,now:u64)->Option<Proposal> {
        if !self.pending.as_ref().is_some_and(|p|p.token==token && now<p.expires_at) {return None;}
        self.pending.take()
    }
}
pub fn snapshot(st:&AppState)->Vec<Row> {
    let now=ramwarden_core::history::unix_now() as i64;
    let mut rows={let hub=st.hub.lock().unwrap();crate::browser::snapshot(&hub.reg,st.cfg.thresholds.inactivity_minutes as i64)};
    for r in &mut rows {
        let feedback=st.history.lock().ok().and_then(|h|h.tab_protection(&r.target.url,now).ok());
        match feedback {
            Some((until,n)) if until<=now => {r.p_return=(r.p_return+f64::from(n.min(4))*0.1).min(1.0);}
            _ => {r.eligible=false;r.closeable=false;r.auto_closeable=false;r.status="protected";r.reason="kept or reopened recently; retain for reuse".into();}
        }
    }
    for row in &mut rows {
        if st.voice.focus.matches(&row.title, &row.target.url, now as u64) {
            row.auto_closeable = false;
            row.eligible = false;
            row.status = "protected";
            row.reason = "matches your current focus; kept out of automatic cleanup".into();
        }
    }
    rows
}
pub fn routes()->Router<AppState> {
    Router::new().route("/browser/review",get(review)).route("/browser/review/respond",post(respond))
        .route("/browser/feedback",post(feedback))
}
async fn review(State(st):State<AppState>)->Json<serde_json::Value> {
    if st.cfg.tabs.close!="live" || st.on_hold() {return Json(serde_json::json!({"proposal":null}));}
    let rows=snapshot(&st);
    let auto=crate::browser::close_candidates(&rows,&st.tabs_config());
    let uncertain=rows.into_iter().filter(|r|r.closeable && !auto.contains(&r.target)
        && matches!(r.status,"candidate"|"unloaded"|"retained")).take(10).collect();
    let proposal=st.hub.lock().unwrap().tab_review.offer(uncertain,ramwarden_core::history::unix_now());
    Json(serde_json::json!({"proposal":proposal}))
}
#[derive(Deserialize)]
struct Response {token:String, action:String, #[serde(default)] tabs:Vec<Target>}
async fn respond(State(st):State<AppState>,Json(body):Json<Response>)->Result<Json<serde_json::Value>,StatusCode> {
    if !matches!(body.action.as_str(),"close"|"keep"|"later") {return Err(StatusCode::BAD_REQUEST);}
    let proposal={let mut hub=st.hub.lock().unwrap();
        let p=hub.tab_review.pending.as_ref().ok_or(StatusCode::CONFLICT)?;
        if body.tabs.iter().any(|t|!p.tabs.iter().any(|r|&r.target==t)) {return Err(StatusCode::BAD_REQUEST);}
        hub.tab_review.consume(&body.token,ramwarden_core::history::unix_now()).ok_or(StatusCode::CONFLICT)?};
    if body.action=="keep" {
        crate::decisionlog::record_user_keep(&st,&proposal.tabs,&body.tabs);
        for t in &body.tabs {
            st.history.lock().map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?.tab_feedback(&t.url,"keep",ramwarden_core::history::unix_now() as i64).map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?;
            st.hub.lock().unwrap().send(&t.browser,&serde_json::json!({"action":"keep_tabs","tabs":[t]}));
        }
    }
    if body.action=="close" {
        if st.on_hold() {return Err(StatusCode::CONFLICT);}
        let result=st.close_safe(&body.tabs,true).await;
        return Ok(Json(serde_json::json!({"confirmed":result.confirmed,"refused":result.refused})));
    }
    Ok(Json(serde_json::json!({"ok":true})))
}
#[derive(Deserialize)]
struct Feedback {url:String,kind:String}
async fn feedback(State(st):State<AppState>,Json(body):Json<Feedback>)->Result<StatusCode,StatusCode> {
    if body.url.len()>8192 || !matches!(body.kind.as_str(),"reopened"|"manual_close"|"confirmed") {return Err(StatusCode::BAD_REQUEST);}
    let url=url::Url::parse(&body.url).map_err(|_|StatusCode::BAD_REQUEST)?;
    if !matches!(url.scheme(),"http"|"https") {return Err(StatusCode::BAD_REQUEST);}
    let h=st.history.lock().map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?;
    h.tab_feedback(&body.url,&body.kind,ramwarden_core::history::unix_now() as i64).map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?;
    if body.kind=="reopened" { crate::decisionlog::note_reopened(&h,&body.url); }
    Ok(StatusCode::NO_CONTENT)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn row()->Row {crate::browser::rank("b",&[crate::tabs::Tab{id:1,url:"https://example.com/".into(),..Default::default()}],30).remove(0)}
    #[test] fn one_shot_expiry_and_throttle() {
        let mut s=ReviewState::default();let p=s.offer(vec![row()],2000).unwrap();
        assert!(s.consume("wrong",2001).is_none()); assert!(s.consume(&p.token,2001).is_some());
        assert!(s.consume(&p.token,2002).is_none()); assert!(s.offer(vec![row()],2100).is_none());
        let p=s.offer(vec![row()],4000).unwrap();assert!(s.consume(&p.token,4600).is_none());
    }
}
