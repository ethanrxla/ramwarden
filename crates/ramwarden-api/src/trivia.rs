//! One shared publisher; public feed reads can never trigger model generation.
use std::{path::PathBuf,sync::{Arc,OnceLock,RwLock},time::Duration};
use axum::{Json,http::{StatusCode,header},response::{Response,IntoResponse}};
use ramwarden_ai::trivia::Feed;
static SERVICE:OnceLock<Arc<Trivia>>=OnceLock::new();
struct Trivia {path:Option<PathBuf>,feed:RwLock<Option<Feed>>}
impl Trivia {
 fn load(path:Option<PathBuf>)->Self {
  let feed=path.as_ref().and_then(|p|std::fs::read(p).ok()).filter(|bytes|bytes.len()<=65536).and_then(|bytes|serde_json::from_slice::<Feed>(&bytes).ok()).filter(|f|f.valid(crate::routes::unix_now()));
  Self{path,feed:RwLock::new(feed)}
 }
 async fn publish(&self){
  let now=crate::routes::unix_now();
  if self.feed.read().unwrap().as_ref().is_some_and(|f|f.valid(now)){return}
  let Some(path)=&self.path else{return};let Some(parent)=path.parent() else{return};
  if tokio::fs::create_dir_all(parent).await.is_err(){return}
  // Shared filesystem lock also guards two publishers using the same mounted cache.
  // A stale lock after a crash needs operator removal; reads never spend money.
  let lock_path=path.with_extension("lock");
  let Ok(_file)=std::fs::OpenOptions::new().write(true).create_new(true).open(&lock_path) else{return};
  let _lock=PublishLock(lock_path);
  // Another publisher may have refreshed the shared disk while this process waited.
  if let Ok(bytes)=tokio::fs::read(path).await && let Ok(feed)=serde_json::from_slice::<Feed>(&bytes) && feed.valid(now){*self.feed.write().unwrap()=Some(feed);return}
  let recent=self.feed.read().unwrap().as_ref().map(|feed|feed.facts.iter().map(|f|f.text.clone()).collect::<Vec<_>>()).unwrap_or_default();
  let Some(facts)=ramwarden_ai::trivia::generate(&recent,now/86400).await else{tracing::warn!("public trivia generation unavailable; retaining cached feed");return};
  let feed=Feed{generated_at:now,expires_at:now+86400,facts};
  let Ok(bytes)=serde_json::to_vec(&feed) else{return};
  if bytes.len()>65536{return}
  // Atomic persistence: a failed write never publishes a partial feed.
  let Ok(mut file)=tempfile::NamedTempFile::new_in(parent) else{return};use std::io::Write;
  if file.write_all(&bytes).is_err() || file.persist(path).is_err(){return}
  tracing::info!(entries=feed.facts.len(),"shared public trivia published");*self.feed.write().unwrap()=Some(feed);
 }
}
struct PublishLock(PathBuf);
impl Drop for PublishLock {fn drop(&mut self){let _=std::fs::remove_file(&self.0);}}
fn service()->Arc<Trivia>{SERVICE.get_or_init(||Arc::new(Trivia::load(std::env::var_os("RAMWARDEN_PUBLIC_TRIVIA_FILE").map(PathBuf::from)))).clone()}
pub fn spawn(){let shared=service();if shared.path.is_none(){return}tokio::spawn(async move{loop{shared.publish().await;tokio::time::sleep(Duration::from_secs(3600)).await;}});}
pub async fn feed()->Response {
 let shared=service();let feed=shared.feed.read().unwrap().clone();
 match feed.filter(|f|f.valid(crate::routes::unix_now())){
  Some(feed)=>([(header::CACHE_CONTROL,"public, max-age=3600")],Json(feed)).into_response(),
  None=>(StatusCode::SERVICE_UNAVAILABLE,[(header::RETRY_AFTER,"3600")],"Public trivia is not ready").into_response(),
 }
}
#[cfg(test)] mod tests {
 use super::*;
 #[tokio::test] async fn two_readers_reuse_one_saved_feed(){
  let d=tempfile::tempdir().unwrap();let path=d.path().join("public.json");let now=crate::routes::unix_now();
  let data=serde_json::to_vec(&Feed{generated_at:now,expires_at:now+86400,facts:vec![ramwarden_ai::trivia::Fact{text:"My horns are a cache miss.".into(),kind:"joke".into(),source_url:None}]}).unwrap();std::fs::write(&path,&data).unwrap();
  let a=Trivia::load(Some(path.clone()));let b=Trivia::load(Some(path.clone()));a.publish().await;b.publish().await;
  assert_eq!(std::fs::read(&path).unwrap(),data);assert_eq!(a.feed.read().unwrap().as_ref().unwrap().generated_at,b.feed.read().unwrap().as_ref().unwrap().generated_at);
 }
 #[test] fn incomplete_cache_is_not_loaded(){let d=tempfile::tempdir().unwrap();let p=d.path().join("facts.json");std::fs::write(&p,"{partial").unwrap();assert!(Trivia::load(Some(p)).feed.read().unwrap().is_none());}
 #[test] fn public_reads_do_not_start_a_publisher(){let service=Trivia::load(None);assert!(service.path.is_none());assert!(service.feed.read().unwrap().is_none());}
}
