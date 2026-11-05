//! Guest identity and encrypted peer messaging. No account or calendar credentials.
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hkdf::Hkdf;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use x25519_dalek::{PublicKey, StaticSecret};
pub type Result<T> = std::result::Result<T, String>;
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}
fn bytes32(value: &str) -> Result<[u8; 32]> {
    B64.decode(value)
        .map_err(|_| "Invalid public key")?
        .try_into()
        .map_err(|_| "Invalid public key".into())
}
#[derive(Serialize, Deserialize)]
struct KeyFile {
    v: u8,
    signing: String,
    encryption: String,
}
pub struct Identity {
    signing: SigningKey,
    encryption: StaticSecret,
    folder: PathBuf,
}
impl Identity {
    pub fn load_or_create(folder: &Path) -> Result<Self> {
        std::fs::create_dir_all(folder)
            .map_err(|_| "Cannot create private agent identity folder")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| "Cannot protect agent identity folder")?;
        }
        let path = folder.join("identity.json");
        if !path.exists() {
            let file = KeyFile {
                v: 1,
                signing: B64.encode(random::<32>()),
                encryption: B64.encode(random::<32>()),
            };
            let mut temporary =
                tempfile::NamedTempFile::new_in(folder).map_err(|_| "Cannot stage identity")?;
            use std::io::Write;
            temporary
                .write_all(&serde_json::to_vec(&file).map_err(|_| "Cannot encode identity")?)
                .map_err(|_| "Cannot save identity")?;
            temporary
                .as_file()
                .sync_all()
                .map_err(|_| "Cannot save identity")?;
            if let Err(error) = temporary.persist_noclobber(&path)
                && error.error.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err("Cannot persist agent identity".into());
            }
        }
        let meta = std::fs::symlink_metadata(&path).map_err(|_| "Cannot inspect agent identity")?;
        if !meta.is_file() || meta.len() > 2048 {
            return Err("Agent identity file is invalid; it was not replaced".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err("Agent identity permissions must be private".into());
            }
        }
        let file: KeyFile =
            serde_json::from_slice(&std::fs::read(path).map_err(|_| "Cannot read agent identity")?)
                .map_err(
                    |_| "Agent identity is damaged; restore it instead of silently replacing it",
                )?;
        if file.v != 1 {
            return Err("Unsupported agent identity version".into());
        }
        Ok(Self {
            signing: SigningKey::from_bytes(&bytes32(&file.signing)?),
            encryption: StaticSecret::from(bytes32(&file.encryption)?),
            folder: folder.to_owned(),
        })
    }
    // Persist ciphertext before delivery so retries never change an idempotent message.
    fn outgoing(&self, peer: &Peer, id: &str, text: &str) -> Result<Value> {
        if id.is_empty() || id.len() > 100 || text.len() > 12000 {
            return Err("Message exceeds its limit".into());
        }
        let folder = self.folder.join("outbox");
        std::fs::create_dir_all(&folder).map_err(|_| "Cannot prepare private outbox")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| "Cannot protect outbox")?;
        }
        let path = folder.join(format!("{:x}.json", Sha256::digest(id.as_bytes())));
        let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
        if !path.exists() {
            // Bound local ciphertext storage; never silently reuse an old message ID.
            let count = std::fs::read_dir(&folder)
                .map_err(|_| "Cannot inspect outbox")?
                .take(2048)
                .count();
            if count >= 2048 {
                return Err("Local outbox is full; archive it before sending more messages".into());
            }
            let record = json!({"recipient":peer.did,"textHash":digest,"envelope":encrypt(&self.did(),peer,id,text)?});
            let mut temp = tempfile::NamedTempFile::new_in(&folder)
                .map_err(|_| "Cannot stage outgoing message")?;
            use std::io::Write;
            temp.write_all(
                &serde_json::to_vec(&record).map_err(|_| "Cannot encode outgoing message")?,
            )
            .map_err(|_| "Cannot save outgoing message")?;
            temp.as_file()
                .sync_all()
                .map_err(|_| "Cannot save outgoing message")?;
            if let Err(error) = temp.persist_noclobber(&path)
                && error.error.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err("Cannot persist outgoing message".into());
            }
        }
        let meta =
            std::fs::symlink_metadata(&path).map_err(|_| "Cannot inspect outgoing message")?;
        if !meta.is_file() || meta.len() > 24000 {
            return Err("Invalid outgoing message cache".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err("Outgoing message permissions must be private".into());
            }
        }
        let record: Value = serde_json::from_slice(
            &std::fs::read(&path).map_err(|_| "Cannot read outgoing message")?,
        )
        .map_err(|_| "Invalid outgoing message cache")?;
        if record["recipient"] != peer.did || record["textHash"] != digest {
            return Err("Message ID was already used for different content".into());
        }
        record
            .get("envelope")
            .cloned()
            .ok_or("Missing outgoing envelope".into())
    }
    pub fn public_key(&self) -> String {
        B64.encode(self.signing.verifying_key().as_bytes())
    }
    pub fn did(&self) -> String {
        identity_id(&self.public_key()).expect("own key")
    }
    pub fn box_key(&self) -> String {
        B64.encode(PublicKey::from(&self.encryption).as_bytes())
    }
}
pub fn identity_id(public_key: &str) -> Result<String> {
    Ok(format!(
        "did:agn:{}",
        B64.encode(Sha256::digest(bytes32(public_key)?))
    ))
}
pub fn canonical(value: &Value) -> Result<String> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => {
            serde_json::to_string(value).map_err(|_| "Invalid message".into())
        }
        Value::Number(n)
            if n.as_i64()
                .is_some_and(|v| v.unsigned_abs() <= 9_007_199_254_740_991)
                || n.as_u64().is_some_and(|v| v <= 9_007_199_254_740_991) =>
        {
            Ok(n.to_string())
        }
        Value::Array(items) => Ok(format!(
            "[{}]",
            items
                .iter()
                .map(canonical)
                .collect::<Result<Vec<_>>>()?
                .join(",")
        )),
        Value::Object(items) => {
            let mut keys = items.keys().collect::<Vec<_>>();
            keys.sort();
            Ok(format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| Ok(format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        canonical(&items[key])?
                    )))
                    .collect::<Result<Vec<_>>>()?
                    .join(",")
            ))
        }
        _ => Err("Only integer JSON values are accepted in signed requests".into()),
    }
}
fn proof_bytes(
    audience: &str,
    method: &str,
    key: &str,
    at: u64,
    nonce: &str,
    params: &Value,
) -> Result<Vec<u8>> {
    Ok(format!(
        "warden-network-v1\n{audience}\n{method}\n{key}\n{at}\n{nonce}\n{:x}",
        Sha256::digest(canonical(params)?.as_bytes())
    )
    .into_bytes())
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Peer {
    pub did: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    #[serde(rename = "boxKey")]
    pub box_key: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub registration: Value,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Pair {
    pub id: String,
    pub state: String,
    #[serde(rename = "needsApproval")]
    pub needs_approval: bool,
    pub peer: Peer,
}
impl Peer {
    pub fn verify(&self, audience: &str) -> Result<()> {
        if identity_id(&self.public_key)? != self.did
            || self.registration["publicKey"] != self.public_key
            || self.registration["audience"] != audience
            || self.registration["method"] != "network/register"
            || self.registration["params"]["boxKey"] != self.box_key
            || self.registration["params"]["displayName"] != self.display_name
        {
            return Err("Peer identity proof does not match its profile".into());
        }
        let at = self.registration["at"]
            .as_u64()
            .ok_or("Invalid peer proof")?;
        let nonce = self.registration["nonce"]
            .as_str()
            .ok_or("Invalid peer proof")?;
        let signature = Signature::from_slice(
            &B64.decode(
                self.registration["signature"]
                    .as_str()
                    .ok_or("Invalid peer proof")?,
            )
            .map_err(|_| "Invalid peer proof")?,
        )
        .map_err(|_| "Invalid peer proof")?;
        VerifyingKey::from_bytes(&bytes32(&self.public_key)?)
            .map_err(|_| "Invalid peer key")?
            .verify_strict(
                &proof_bytes(
                    audience,
                    "network/register",
                    &self.public_key,
                    at,
                    nonce,
                    &self.registration["params"],
                )?,
                &signature,
            )
            .map_err(|_| "Peer signature verification failed".into())
    }
}
pub struct Client {
    http: reqwest::Client,
    endpoint: reqwest::Url,
    pub identity: Identity,
}
impl Client {
    pub fn new(endpoint: &str, identity: Identity) -> Result<Self> {
        let url = reqwest::Url::parse(endpoint).map_err(|_| "Invalid agent network address")?;
        let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        if !(url.scheme() == "https" && url.host_str() == Some("scholarcal.com")
            || url.scheme() == "http" && local)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/api/agent-network/v1"
        {
            return Err("Agent Network requires its trusted HTTPS endpoint or explicit local development endpoint".into());
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "Cannot prepare network connection")?;
        Ok(Self {
            http,
            endpoint: url,
            identity,
        })
    }
    pub fn audience(&self) -> String {
        self.endpoint.origin().ascii_serialization()
    }
    pub async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let at = now();
        let nonce = random::<16>()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let key = self.identity.public_key();
        let audience = self.audience();
        let signature = B64.encode(
            self.identity
                .signing
                .sign(&proof_bytes(&audience, method, &key, at, &nonce, &params)?)
                .to_bytes(),
        );
        let response=self.http.post(self.endpoint.clone()).header("x-warden-audience",audience).header("x-warden-key",key).header("x-warden-time",at).header("x-warden-nonce",nonce).header("x-warden-signature",signature)
   .json(&json!({"jsonrpc":"2.0","id":uuid::Uuid::new_v4().to_string(),"method":method,"params":params})).send().await.map_err(|_|"Agent Network could not be reached")?;
        let status = response.status();
        if response.content_length().is_some_and(|n| n > 524288) {
            return Err("Agent Network response exceeds its limit".into());
        }
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Agent Network response interrupted")?
        {
            if bytes.len() + chunk.len() > 524288 {
                return Err("Agent Network response exceeds its limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "Agent Network returned an invalid response")?;
        if !status.is_success() || value.get("error").is_some() {
            return Err(match value["error"]["message"].as_str() {
                Some("NOT_PAIRED") => "Approve this peer pairing before messaging",
                Some("INVITE_UNAVAILABLE") => "The pairing invitation expired or was already used",
                Some("RATE_LIMIT") => "Agent Network usage limit reached; retry later",
                Some("REPLAY") => "This request was already used; retry with a fresh request",
                _ => "Agent Network is not available for this operation yet",
            }
            .into());
        }
        value
            .get("result")
            .cloned()
            .ok_or("Missing Agent Network result".into())
    }
    pub async fn register(&self, name: &str) -> Result<String> {
        let result = self
            .rpc(
                "network/register",
                json!({"displayName":name,"boxKey":self.identity.box_key()}),
            )
            .await?;
        if result["did"] != self.identity.did()
            || result["publicKey"] != self.identity.public_key()
            || result["boxKey"] != self.identity.box_key()
        {
            return Err("Agent Network returned a different identity".into());
        }
        Ok(self.identity.did())
    }
    pub async fn pairs(&self) -> Result<Vec<Pair>> {
        let result = self.rpc("pairs/list", json!({})).await?;
        let pairs: Vec<Pair> =
            serde_json::from_value(result["pairs"].clone()).map_err(|_| "Invalid peer list")?;
        if pairs.len() > 20 {
            return Err("Peer list exceeds its limit".into());
        }
        for pair in &pairs {
            pair.peer.verify(&self.audience())?;
        }
        Ok(pairs)
    }
    pub async fn send(&self, peer: &Peer, id: &str, text: &str) -> Result<Value> {
        peer.verify(&self.audience())?;
        let envelope = self.identity.outgoing(peer, id, text)?;
        self.rpc("message/send",json!({"message":{"messageId":id,"role":"user","parts":[{"kind":"data","data":envelope}],"metadata":{"recipient":peer.did}}})).await
    }
    pub fn decrypt_task(&self, task: &Value, sender: &Peer) -> Result<String> {
        sender.verify(&self.audience())?;
        let from = task["metadata"]["sender"]
            .as_str()
            .ok_or("Missing sender")?;
        let to = task["metadata"]["recipient"]
            .as_str()
            .ok_or("Missing recipient")?;
        if from != sender.did || to != self.identity.did() {
            return Err("Message does not belong to this pairing".into());
        }
        let id = task["metadata"]["clientMessageId"]
            .as_str()
            .ok_or("Missing message identity")?;
        decrypt(
            &self.identity.encryption,
            from,
            to,
            id,
            &task["artifacts"][0]["parts"][0]["data"],
        )
    }
}
fn message_key(shared: &[u8], from: &str, to: &str) -> Result<[u8; 32]> {
    let mut key = [0; 32];
    Hkdf::<Sha256>::new(None, shared)
        .expand(
            format!("warden-message-v1\n{from}\n{to}").as_bytes(),
            &mut key,
        )
        .map_err(|_| "Cannot prepare message key")?;
    Ok(key)
}
fn associated(from: &str, to: &str, id: &str) -> Vec<u8> {
    format!("warden-message-v1\n{from}\n{to}\n{id}").into_bytes()
}
fn encrypt(from: &str, peer: &Peer, id: &str, text: &str) -> Result<Value> {
    if text.len() > 12000 || id.is_empty() || id.len() > 100 {
        return Err("Message exceeds its limit".into());
    }
    let ephemeral = StaticSecret::from(random::<32>());
    let shared = ephemeral.diffie_hellman(&PublicKey::from(bytes32(&peer.box_key)?));
    if !shared.was_contributory() {
        return Err("Invalid peer encryption key".into());
    }
    let key = message_key(shared.as_bytes(), from, &peer.did)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| "Cannot prepare encrypted message")?;
    let nonce = random::<12>();
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: text.as_bytes(),
                aad: &associated(from, &peer.did, id),
            },
        )
        .map_err(|_| "Message encryption failed")?;
    Ok(
        json!({"v":1,"ephemeralKey":B64.encode(PublicKey::from(&ephemeral).as_bytes()),"nonce":B64.encode(nonce),"ciphertext":B64.encode(ciphertext)}),
    )
}
fn decrypt(
    secret: &StaticSecret,
    from: &str,
    to: &str,
    id: &str,
    envelope: &Value,
) -> Result<String> {
    if envelope["v"] != 1 {
        return Err("Unsupported encrypted message version".into());
    }
    let public = bytes32(
        envelope["ephemeralKey"]
            .as_str()
            .ok_or("Invalid encrypted message")?,
    )?;
    let shared = secret.diffie_hellman(&PublicKey::from(public));
    if !shared.was_contributory() {
        return Err("Invalid encryption key".into());
    }
    let nonce = B64
        .decode(
            envelope["nonce"]
                .as_str()
                .ok_or("Invalid encrypted message")?,
        )
        .map_err(|_| "Invalid message nonce")?;
    if nonce.len() != 12 {
        return Err("Invalid message nonce".into());
    }
    let bytes = B64
        .decode(
            envelope["ciphertext"]
                .as_str()
                .ok_or("Invalid encrypted message")?,
        )
        .map_err(|_| "Invalid encrypted message")?;
    if bytes.len() > 16000 {
        return Err("Message exceeds its limit".into());
    }
    let key = message_key(shared.as_bytes(), from, to)?;
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|_| "Cannot prepare message decryption")?;
    let plain = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &bytes,
                aad: &associated(from, to, id),
            },
        )
        .map_err(|_| "Message authentication failed")?;
    String::from_utf8(plain).map_err(|_| "Message is not valid text".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn peer(identity: &Identity) -> Peer {
        Peer {
            did: identity.did(),
            public_key: identity.public_key(),
            box_key: identity.box_key(),
            display_name: "Test".into(),
            registration: Value::Null,
        }
    }
    #[test]
    fn identity_survives_restart_and_damage_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let first = Identity::load_or_create(dir.path()).unwrap().did();
        assert_eq!(first, Identity::load_or_create(dir.path()).unwrap().did());
        std::fs::write(dir.path().join("identity.json"), b"broken").unwrap();
        assert!(Identity::load_or_create(dir.path()).is_err());
        assert_eq!(
            std::fs::read(dir.path().join("identity.json")).unwrap(),
            b"broken"
        );
    }
    #[test]
    fn encrypted_retry_survives_restart_and_rejects_id_reuse() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let sender = Identity::load_or_create(a.path()).unwrap();
        let receiver = Identity::load_or_create(b.path()).unwrap();
        let recipient = peer(&receiver);
        let envelope = sender
            .outgoing(&recipient, "message-1", "hello goat")
            .unwrap();
        let restarted = Identity::load_or_create(a.path()).unwrap();
        assert_eq!(
            envelope,
            restarted
                .outgoing(&recipient, "message-1", "hello goat")
                .unwrap()
        );
        assert!(
            restarted
                .outgoing(&recipient, "message-1", "different")
                .is_err()
        );
        assert_eq!(
            decrypt(
                &receiver.encryption,
                &sender.did(),
                &recipient.did,
                "message-1",
                &envelope
            )
            .unwrap(),
            "hello goat"
        );
        assert!(
            decrypt(
                &receiver.encryption,
                &sender.did(),
                &recipient.did,
                "message-2",
                &envelope
            )
            .is_err()
        );
        assert!(
            decrypt(
                &receiver.encryption,
                &recipient.did,
                &sender.did(),
                "message-1",
                &envelope
            )
            .is_err()
        );
    }
    #[test]
    fn canonical_sorting_and_integer_restriction() {
        assert_eq!(
            canonical(&json!({"z":[true,null],"a":"goat"})).unwrap(),
            r#"{"a":"goat","z":[true,null]}"#
        );
        assert!(canonical(&json!({"x":1.25})).is_err());
    }
    #[test]
    fn production_endpoint_is_fixed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Client::new(
                "https://evil.example/api/agent-network/v1",
                Identity::load_or_create(dir.path()).unwrap()
            )
            .is_err()
        );
    }
}
