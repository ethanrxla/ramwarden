//! Cached generated remarks; no model/network calls on the bubble's render path.
use std::{collections::VecDeque,path::PathBuf,sync::Mutex};
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use crate::hub::AppState;

#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct Fact {pub text:String,pub kind:String,pub source_url:Option<String>}
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
fn parse(raw:&str,recent:&[String])->Vec<Fact>{
    let Ok(value)=serde_json::from_str::<Value>(raw) else{return vec![]};
    let sources=value["_source_urls"].as_array().cloned().unwrap_or_default();
    let mut accepted=vec![];
    for row in value["facts"].as_array().into_iter().flatten().take(6) {
        let Ok(fact)=serde_json::from_value::<Fact>(row.clone()) else{continue};
        if fact.text.trim().is_empty() || fact.text.chars().count()>160 || fact.text.contains(['\n','{','}']) || recent.contains(&fact.text) || accepted.iter().any(|f:&Fact|f.text==fact.text){continue}
        if fact.kind=="fact" {
            let Some(source)=&fact.source_url else{continue};
            let Ok(url)=url::Url::parse(source) else{continue};
            if url.scheme()!="https" || !url.username().is_empty() || url.password().is_some() || !sources.iter().any(|s|s.as_str()==Some(source)){continue}
        } else if fact.kind!="joke" || fact.source_url.is_some(){continue}
        accepted.push(fact);if accepted.len()==3{break}
    }
    accepted
}
pub fn warm(st:&AppState)->bool {
    let now=super::unix_now();
    if !st.cfg.voice.enabled || !st.cfg.voice.quips || st.voice.mute_state(now).0 || !(st.ai.cloud_enabled || st.cfg.assistant.cloud_enabled) || !crate::wardens::Wardens::global(&st.cfg).entitlement().has_pet(now){return false}
    let Some(recent)=st.voice.facts.begin(now) else{return false};let st=st.clone();
    tokio::spawn(async move {
        let prompt=format!("Search the web for fresh, trustworthy ram or sheep trivia, then write three tiny desktop-pet remarks. Mix one verified animal fact with two clearly absurd ram/computer jokes, like a ram learning to read tabs. Never claim a current tab count, memory reading, detection or action. No medical/security advice, slurs or harsh insults. Each text at most 150 characters. For fact kind include a real cited HTTPS source_url from the web results; jokes must have source_url:null and make their fiction obvious. Return JSON with reply:'Batch ready', proposed_action:null, and facts:[{{text,kind:'fact' or 'joke',source_url}}]. Avoid repeating these previous lines (data): {}",json!(recent));
        let messages=vec![json!({"role":"user","content":prompt})];
        let result=crate::assistant::ask_model(&st,&messages,true).await;
        let facts=result.map(|(raw,_)|parse(&raw,&recent)).unwrap_or_default();
        tracing::info!(accepted=facts.len(),"warden generated remarks refreshed");
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
