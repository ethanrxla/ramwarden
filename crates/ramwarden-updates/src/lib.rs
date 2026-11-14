//! Pinned signed metadata and verified installer downloads. No silent installation.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub type Result<T> = std::result::Result<T, String>;
pub const MANIFEST_URL: &str =
    "https://github.com/ethanrxla/ramwarden/releases/latest/download/updates.json";
pub const PUBLIC_KEY: Option<&str> = option_env!("RAMWARDEN_UPDATE_PUBLIC_KEY");
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub payload: String,
    pub signature: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub channel: String,
    pub version: String,
    pub published_at: u64,
    pub expires_at: u64,
    pub notes: String,
    pub artifacts: Vec<Artifact>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub target: String,
    pub filename: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub current_version: String,
    pub checked_at: u64,
    pub available_version: Option<String>,
    pub notes: Option<String>,
    pub error: Option<String>,
    pub configured: bool,
}
pub fn target() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unsupported"
    }
}
fn approved(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
        && url.username().is_empty()
        && url.password().is_none()
}
fn artifact_valid(a: &Artifact) -> bool {
    let expected = match a.target.as_str() {
        "aarch64-apple-darwin" | "x86_64-apple-darwin" => "RamWarden.dmg",
        "x86_64-pc-windows-msvc" => "ramwarden-windows-x86_64.exe",
        _ => return false,
    };
    a.filename == expected
        && a.size > 0
        && a.size <= 512 * 1024 * 1024
        && a.sha256.len() == 64
        && a.sha256.bytes().all(|c| c.is_ascii_hexdigit())
        && reqwest::Url::parse(&a.url).is_ok_and(|u| {
            approved(&u)
                && u.host_str() == Some("github.com")
                && u.path()
                    .starts_with("/ethanrxla/ramwarden/releases/download/")
                && u.path().ends_with(&format!("/{}", a.filename))
                && u.query().is_none()
                && u.fragment().is_none()
        })
}
pub fn verify(envelope: &Envelope, key: &str, at: u64) -> Result<Manifest> {
    if envelope.payload.len() > 90000 || envelope.signature.len() > 100 {
        return Err("Update metadata exceeds its limit".into());
    }
    let payload = STANDARD
        .decode(&envelope.payload)
        .map_err(|_| "Invalid update encoding")?;
    let public: [u8; 32] = STANDARD
        .decode(key)
        .map_err(|_| "Invalid update trust key")?
        .try_into()
        .map_err(|_| "Invalid update trust key")?;
    let signature = Signature::from_slice(
        &STANDARD
            .decode(&envelope.signature)
            .map_err(|_| "Invalid update signature")?,
    )
    .map_err(|_| "Invalid update signature")?;
    VerifyingKey::from_bytes(&public)
        .map_err(|_| "Invalid update trust key")?
        .verify_strict(&payload, &signature)
        .map_err(|_| "Update signature verification failed")?;
    let manifest: Manifest =
        serde_json::from_slice(&payload).map_err(|_| "Invalid update metadata")?;
    if manifest.schema != 1
        || manifest.channel != "stable"
        || manifest.published_at > at + 300
        || manifest.expires_at <= at
        || manifest.expires_at <= manifest.published_at
        || manifest.expires_at - manifest.published_at > 30 * 86400
        || manifest.notes.len() > 8192
        || manifest.artifacts.is_empty()
        || manifest.artifacts.len() > 3
        || semver::Version::parse(&manifest.version).is_err()
    {
        return Err("Update metadata is expired or invalid".into());
    }
    let mut targets = std::collections::HashSet::new();
    if manifest
        .artifacts
        .iter()
        .any(|a| !artifact_valid(a) || !targets.insert(a.target.clone()))
    {
        return Err("Update artifacts are invalid or duplicated".into());
    }
    Ok(manifest)
}
pub fn newer(manifest: &Manifest, current: &str) -> Result<bool> {
    Ok(
        semver::Version::parse(&manifest.version).map_err(|_| "Invalid release version")?
            > semver::Version::parse(current).map_err(|_| "Invalid installed version")?,
    )
}
fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 5 && approved(attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .user_agent(concat!("RamWarden-updater/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| "Update connection unavailable".into())
}
async fn bounded(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if !response.status().is_success() {
        return Err(if response.status() == 404 {
            "No signed update channel has been published yet"
        } else {
            "Update service unavailable"
        }
        .into());
    }
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err("Update download exceeds its limit".into());
    }
    let mut response = response;
    let mut bytes = vec![];
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Update download interrupted")?
    {
        if bytes.len() + chunk.len() > limit {
            return Err("Update download exceeds its limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
pub async fn fetch(key: &str) -> Result<(Envelope, Manifest)> {
    let response = http()?
        .get(MANIFEST_URL)
        .send()
        .await
        .map_err(|_| "Cannot reach the update service")?;
    let bytes = bounded(response, 128 * 1024).await?;
    let envelope: Envelope =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid signed update envelope")?;
    let manifest = verify(&envelope, key, now())?;
    Ok((envelope, manifest))
}
pub fn store() -> PathBuf {
    ramwarden_core::config::data_dir().join("ramwarden/updates")
}
fn save(envelope: &Envelope) -> Result<()> {
    let dir = store();
    std::fs::create_dir_all(&dir).map_err(|_| "Cannot save update state")?;
    let mut file =
        tempfile::NamedTempFile::new_in(&dir).map_err(|_| "Cannot stage update state")?;
    use std::io::Write;
    file.write_all(&serde_json::to_vec(envelope).map_err(|_| "Cannot serialize update state")?)
        .map_err(|_| "Cannot save update state")?;
    file.persist(dir.join("verified-manifest.json"))
        .map_err(|_| "Cannot save update state")?;
    Ok(())
}
pub fn cached() -> Option<Manifest> {
    let bytes = std::fs::read(store().join("verified-manifest.json")).ok()?;
    if bytes.len() > 128 * 1024 {
        return None;
    }
    let envelope: Envelope = serde_json::from_slice(&bytes).ok()?;
    verify(&envelope, PUBLIC_KEY?, now()).ok()
}
pub async fn check() -> Status {
    let mut status = Status {
        current_version: env!("CARGO_PKG_VERSION").into(),
        checked_at: now(),
        available_version: None,
        notes: None,
        error: None,
        configured: PUBLIC_KEY.is_some(),
    };
    let Some(key) = PUBLIC_KEY else {
        status.error = Some("This build has no update trust key".into());
        return status;
    };
    match fetch(key).await {
        Ok((envelope, manifest)) => {
            if let Err(error) = save(&envelope) {
                status.error = Some(error);
            }
            if newer(&manifest, &status.current_version).unwrap_or(false) {
                if manifest.artifacts.iter().any(|a| a.target == target()) {
                    status.available_version = Some(manifest.version);
                    status.notes = Some(manifest.notes);
                } else {
                    status.error = Some("No update installer for this platform".into())
                }
            }
        }
        Err(error) => {
            status.error = Some(error);
            if let Some(manifest) =
                cached().filter(|m| newer(m, &status.current_version).unwrap_or(false))
            {
                status.available_version = Some(manifest.version);
                status.notes = Some(manifest.notes);
            }
        }
    }
    status
}
pub fn matches_artifact(bytes: &[u8], artifact: &Artifact) -> bool {
    bytes.len() as u64 == artifact.size
        && format!("{:x}", Sha256::digest(bytes)).eq_ignore_ascii_case(&artifact.sha256)
}
pub async fn download() -> Result<PathBuf> {
    let key = PUBLIC_KEY.ok_or("This build has no update trust key")?;
    let (_, manifest) = fetch(key).await?;
    if !newer(&manifest, env!("CARGO_PKG_VERSION"))? {
        return Err("No newer update is available".into());
    }
    let artifact = manifest
        .artifacts
        .iter()
        .find(|a| a.target == target())
        .ok_or("No installer for this platform")?;
    let response = http()?
        .get(&artifact.url)
        .send()
        .await
        .map_err(|_| "Cannot download the update installer")?;
    let dir = store().join(&manifest.version);
    std::fs::create_dir_all(&dir).map_err(|_| "Cannot create update directory")?;
    if !response.status().is_success() {
        return Err("Update download failed".into());
    }
    let mut response = response;
    let mut file = tempfile::NamedTempFile::new_in(&dir).map_err(|_| "Cannot stage installer")?;
    use std::io::Write;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Update download interrupted")?
    {
        size += chunk.len() as u64;
        if size > artifact.size {
            return Err("Update exceeds declared size".into());
        }
        hash.update(&chunk);
        file.write_all(&chunk)
            .map_err(|_| "Cannot save installer")?;
    }
    if size != artifact.size
        || !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(&artifact.sha256)
    {
        return Err("Update checksum or size did not match; installer was not opened".into());
    }
    file.as_file()
        .sync_all()
        .map_err(|_| "Cannot save installer")?;
    let path = dir.join(&artifact.filename);
    file.persist(&path)
        .map_err(|_| "Cannot save verified installer")?;
    Ok(path)
}
pub fn open(path: &Path) -> Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer.exe"
    } else {
        return Err("Open the downloaded installer manually on this platform".into());
    };
    std::process::Command::new(program)
        .arg(path)
        .spawn()
        .map_err(|_| "Cannot open the verified installer")?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    fn fixture() -> (Envelope, String) {
        let key = SigningKey::from_bytes(&[7; 32]);
        let manifest = Manifest {
            schema: 1,
            channel: "stable".into(),
            version: "1.2.3".into(),
            published_at: 100,
            expires_at: 200,
            notes: "New release".into(),
            artifacts: vec![Artifact {
                target: "aarch64-apple-darwin".into(),
                filename: "RamWarden.dmg".into(),
                url:
                    "https://github.com/ethanrxla/ramwarden/releases/download/v1.2.3/RamWarden.dmg"
                        .into(),
                sha256: format!("{:x}", Sha256::digest(b"fixture")),
                size: 7,
            }],
        };
        let payload = serde_json::to_vec(&manifest).unwrap();
        (
            Envelope {
                signature: STANDARD.encode(key.sign(&payload).to_bytes()),
                payload: STANDARD.encode(payload),
            },
            STANDARD.encode(key.verifying_key().to_bytes()),
        )
    }
    #[test]
    fn signatures_expiry_and_downgrades() {
        let (mut env, key) = fixture();
        let manifest = verify(&env, &key, 150).unwrap();
        assert!(newer(&manifest, "1.0.0").unwrap());
        assert!(!newer(&manifest, "2.0.0").unwrap());
        assert!(verify(&env, &key, 201).is_err());
        env.payload.push('A');
        assert!(verify(&env, &key, 150).is_err());
    }
    #[test]
    fn unsafe_artifacts_and_mismatched_bytes() {
        let (env, key) = fixture();
        let mut m = verify(&env, &key, 150).unwrap();
        assert!(matches_artifact(b"fixture", &m.artifacts[0]));
        assert!(!matches_artifact(b"changed", &m.artifacts[0]));
        m.artifacts[0].url = "http://evil.invalid/file".into();
        assert!(!artifact_valid(&m.artifacts[0]));
        m.artifacts[0].filename = "../escape.exe".into();
        assert!(!artifact_valid(&m.artifacts[0]));
    }
}
