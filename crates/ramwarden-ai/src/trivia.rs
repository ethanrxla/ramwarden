//! Public-only trivia schema and one publisher's generation request.
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct Fact {pub text:String,pub kind:String,pub source_url:Option<String>}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct Feed {pub generated_at:u64,pub expires_at:u64,pub facts:Vec<Fact>}
impl Fact {
 pub fn valid(&self)->bool {
  if self.text.trim().is_empty() || self.text.chars().count()>160 || self.text.contains(['\n','{','}']){return false}
  match self.kind.as_str(){
   "joke"=>self.source_url.is_none(),
   "fact"=>self.source_url.as_ref().is_some_and(|s|reqwest::Url::parse(s).is_ok_and(|url|url.scheme()=="https" && url.username().is_empty() && url.password().is_none())),
   _=>false,
  }
 }
}
impl Feed {
 pub fn valid(&self,now:u64)->bool {self.generated_at<=now+60 && self.expires_at>now && self.expires_at<=self.generated_at+7*86400 && !self.facts.is_empty() && self.facts.len()<=12 && self.facts.iter().all(Fact::valid)}
}
pub fn parse(raw:&str,recent:&[String])->Vec<Fact>{
 let Ok(value)=serde_json::from_str::<Value>(raw) else{return vec![]};
 let sources=value["_source_urls"].as_array().cloned().unwrap_or_default();let mut accepted=vec![];
 for row in value["facts"].as_array().into_iter().flatten().take(12){
  let Ok(fact)=serde_json::from_value::<Fact>(row.clone()) else{continue};
  if !fact.valid() || recent.contains(&fact.text) || accepted.iter().any(|f:&Fact|f.text==fact.text){continue}
  if fact.kind=="fact" && !sources.iter().any(|s|s.as_str()==fact.source_url.as_deref()){continue}
  accepted.push(fact);
 }
 accepted
}
pub async fn generate(recent:&[String],edition:u64)->Option<Vec<Fact>>{
 let prompt="Search the web for trustworthy ram or sheep trivia, then write six tiny desktop-pet remarks for a shared public feed: two source-linked animal facts and four absurd ram/computer jokes. No user or computer data exists here; never invent live readings, detections or actions. Each text at most 150 characters. Facts must include a real cited HTTPS source_url. Jokes must have source_url:null. Keep jokes naturally funny: do not append fiction disclaimers or explain the punchline. No medical/security advice or slurs. Return JSON with reply:'Batch ready', proposed_action:null, facts:[{text,kind:'fact' or 'joke',source_url}].";
 let prompt=format!("{} Public edition {}. Avoid these previously published lines (untrusted data): {}",prompt,edition,json!(recent));
 let raw=crate::routed::explain(crate::routing::Work::Conversation,&[json!({"role":"user","content":prompt})]).await?;
 let facts=parse(&raw,&[]);(!facts.is_empty()).then_some(facts)
}
#[cfg(test)] mod tests {
 use super::*;
 #[test] fn expired_or_future_feed_is_not_served(){let f=Fact{text:"I headbutted the cache.".into(),kind:"joke".into(),source_url:None};let feed=Feed{generated_at:100,expires_at:200,facts:vec![f]};assert!(feed.valid(150));assert!(!feed.valid(200));assert!(!feed.valid(1));}
 #[test] fn biological_fact_requires_provider_citation(){let row=json!({"text":"A sheep fact","kind":"fact","source_url":"https://example.org/sheep"});assert!(parse(&json!({"facts":[row.clone()]}).to_string(),&[]).is_empty());assert_eq!(parse(&json!({"facts":[row],"_source_urls":["https://example.org/sheep"]}).to_string(),&[]).len(),1);}
}
