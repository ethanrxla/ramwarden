//! Development-only signed-client interoperability probe; never uses installed identities.
use ramwarden_agent_network::{Client,Identity};
use serde_json::json;
#[tokio::main]
async fn main()->Result<(),String>{
 let endpoint=std::env::args().nth(1).ok_or("Missing local test endpoint")?;
 if !endpoint.starts_with("http://127.0.0.1:"){return Err("Probe requires an isolated loopback fixture".into())}
 let a=tempfile::tempdir().map_err(|e|e.to_string())?;let b=tempfile::tempdir().map_err(|e|e.to_string())?;
 let alice=Client::new(&endpoint,Identity::load_or_create(a.path())?)?;
 let bob=Client::new(&endpoint,Identity::load_or_create(b.path())?)?;
 alice.register("Alice test agent").await?;bob.register("Bob test agent").await?;
 let invite=alice.rpc("pair/invite",json!({})).await?;
 let accepted=bob.rpc("pair/accept",json!({"invitation":invite["invitation"]})).await?;
 let pending=alice.pairs().await?;assert!(pending[0].needs_approval);
 assert!(alice.send(&pending[0].peer,"before-approval","not allowed").await.is_err());
 alice.rpc("pair/confirm",json!({"id":accepted["id"]})).await?;
 let bob_peer=alice.pairs().await?.remove(0).peer;
 let task=alice.send(&bob_peer,"message-one","Hello from the Rust client 🐐").await?;
 let retried=alice.send(&bob_peer,"message-one","Hello from the Rust client 🐐").await?;
 assert_eq!(task["id"],retried["id"]);
 let inbox=bob.rpc("messages/list",json!({})).await?;
 let alice_peer=bob.pairs().await?.remove(0).peer;
 assert_eq!(bob.decrypt_task(&inbox["tasks"][0],&alice_peer)?,"Hello from the Rust client 🐐");
 bob.rpc("message/ack",json!({"id":task["id"]})).await?;
 alice.rpc("pair/revoke",json!({"id":accepted["id"]})).await?;
 assert!(alice.send(&bob_peer,"after-revoke","not allowed").await.is_err());
 println!("Signed pairing, encrypted delivery, idempotent retry and revocation passed.");Ok(())
}
