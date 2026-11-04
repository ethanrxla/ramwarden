//! Cached generated remarks; no model/network calls on the bubble's render path.
use std::{collections::VecDeque,path::PathBuf,sync::Mutex};
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use crate::hub::AppState;

pub use ramwarden_ai::trivia::Fact;
#[derive(Default,Serialize,Deserialize)]
struct Stored {ready:VecDeque<Fact>,recent:VecDeque<String>,refresh_after:u64}
struct State {stored:Stored,inflight:bool}
pub struct Facts {state:Mutex<State>,path:Option<PathBuf>}
impl Facts {
    pub fn new(path:Option<PathBuf>)->Self {
        let stored=path.as_ref().and_then(|p|std::fs::read(p).ok()).filter(|v|v.len()<32768).and_then(|v|serde_json::from_slice::<Stored>(&v).ok()).unwrap_or_default();
        Self{state:Mutex::new(State{stored,inflight:false}),path}
    }
    pub fn take(&self)->Option<Fact>{
        let mut state=self.state.lock().unwrap();
        let fact=state.stored.ready.pop_front()?;
        state.stored.recent.push_back(fact.text.clone());while state.stored.recent.len()>20 {state.stored.recent.pop_front();}
        let bytes=serde_json::to_vec(&state.stored).ok();let path=self.path.clone();drop(state);
        if let (Some(bytes),Some(path),Ok(runtime))=(bytes,path,tokio::runtime::Handle::try_current()) {
            runtime.spawn_blocking(move||save(&path,&bytes));
        }
        Some(fact)
    }
    pub fn view(&self)->Value {let state=self.state.lock().unwrap();json!({"ready":state.stored.ready,"refresh_after":state.stored.refresh_after,"generating":state.inflight})}
    fn begin(&self,now:u64)->Option<Vec<String>> {
        let mut state=self.state.lock().unwrap();
        if state.inflight || now<state.stored.refresh_after || !state.stored.ready.is_empty(){return None}
        state.inflight=true;state.stored.refresh_after=now+3600;
        Some(state.stored.recent.iter().cloned().collect())
    }
    fn finish(&self,facts:Vec<Fact>,now:u64){
        let snapshot={let mut state=self.state.lock().unwrap();state.inflight=false;
            if !facts.is_empty(){state.stored.ready=facts.into();state.stored.refresh_after=now+86400;}
            serde_json::to_vec(&state.stored).ok()};
        if let (Some(path),Some(bytes))=(&self.path,snapshot){
            // Public trivia only; atomic, tiny cache, never a credential/conversation store.
            save(path,&bytes);
        }
    }
}
fn save(path:&std::path::Path,bytes:&[u8]) {
    if let Some(parent)=path.parent(){let _=std::fs::create_dir_all(parent);if let Ok(mut file)=tempfile::NamedTempFile::new_in(parent){use std::io::Write;if file.write_all(bytes).is_ok(){let _=file.persist(path);}}}
}
#[cfg(test)]
fn parse(raw:&str,recent:&[String])->Vec<Fact>{ramwarden_ai::trivia::parse(raw,recent).into_iter().take(3).collect()}
async fn download(st:&AppState,recent:&[String])->Option<Vec<Fact>> {
 let mut url=reqwest::Url::parse(&st.cfg.wardens.threat.cloud_url).ok()?;
 let loopback=matches!(url.host_str(),Some("localhost"|"127.0.0.1"|"[::1]"));
 if !(url.scheme()=="https" || url.scheme()=="http" && loopback) || !url.username().is_empty() || url.password().is_some(){return None}
 url.set_path("/v1/public/fun-facts");url.set_query(None);url.set_fragment(None);
 let http=reqwest::Client::builder().timeout(std::time::Duration::from_secs(8)).redirect(reqwest::redirect::Policy::none()).build().ok()?;
 let mut response=http.get(url).send().await.ok()?;if !response.status().is_success(){return None}
 let mut bytes=vec![];while let Some(chunk)=response.chunk().await.ok()?{if bytes.len()+chunk.len()>65536{return None}bytes.extend_from_slice(&chunk);}
 let feed=serde_json::from_slice::<ramwarden_ai::trivia::Feed>(&bytes).ok()?;if !feed.valid(super::unix_now()){return None}
 Some(feed.facts.into_iter().filter(|f|!recent.contains(&f.text)).take(3).collect())
}
pub fn warm(st:&AppState)->bool {
    let now=super::unix_now();
    if !st.cfg.voice.enabled || !st.cfg.voice.quips || st.voice.mute_state(now).0 || !(st.ai.cloud_enabled || st.cfg.assistant.cloud_enabled) || !crate::wardens::Wardens::global(&st.cfg).entitlement().has_pet(now){return false}
    let Some(recent)=st.voice.facts.begin(now) else{return false};let st=st.clone();
    tokio::spawn(async move {
        let facts=download(&st,&recent).await.unwrap_or_default();
        tracing::info!(accepted=facts.len(),"shared public remarks downloaded; no local model call");
        st.voice.facts.finish(facts,super::unix_now());
    });true
}
#[cfg(test)]
mod tests {
 use super::*;
 #[test] fn rejects_uncited_facts_repeats_and_oversized_lines(){
  let raw=json!({"facts":[{"text":"Verified sheep fact","kind":"fact","source_url":"https://example.org/sheep"},{"text":"I installed horns instead of more RAM.","kind":"joke","source_url":null},{"text":"x".repeat(161),"kind":"joke","source_url":null}]}).to_string();
  let accepted=parse(&raw,&[]);assert_eq!(accepted.len(),1);assert_eq!(accepted[0].kind,"joke");
  assert!(parse(&raw,&[accepted[0].text.clone()]).is_empty());
 }
 #[test] fn accepts_only_provider_cited_fact_sources(){
  let raw=json!({"_source_urls":["https://example.org/sheep"],"facts":[{"text":"A sourced animal fact.","kind":"fact","source_url":"https://example.org/sheep"}]}).to_string();assert_eq!(parse(&raw,&[]).len(),1);
 }
 #[test] fn cache_is_single_flight_and_rate_limited(){let facts=Facts::new(None);assert!(facts.begin(1).is_some());assert!(facts.begin(2).is_none());facts.finish(vec![],3);assert!(facts.begin(4).is_none());}
}
