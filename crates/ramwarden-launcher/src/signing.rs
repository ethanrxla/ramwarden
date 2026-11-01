//! Ed25519 over the manifest's exact bytes.
//!
//! Detached signature file: one line, base64 of the 64 signature bytes. Keys
//! are base64 of 32 bytes: the public key for verifying, the seed for signing.
//! No PEM, no key ids: the launcher tries every compiled-in key, of which
//! there are at most two (during a rotation).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};

use crate::{Error, Result};

/// The keys shipped in this build, from `update-keys.txt`.
const SHIPPED: &str = include_str!("../update-keys.txt");

/// Checks manifest signatures against a fixed set of public keys.
#[derive(Clone, Debug)]
pub struct Verifier {
    keys: Vec<VerifyingKey>,
}

impl Verifier {
    /// The keys compiled into this build. A line that does not decode is a
    /// build error in spirit; it is skipped and the build's tests catch it.
    pub fn shipped() -> Verifier {
        Verifier::from_lines(SHIPPED).unwrap_or(Verifier { keys: Vec::new() })
    }

    /// Parse `#`-commented lines of base64 public keys.
    pub fn from_lines(text: &str) -> Result<Verifier> {
        let keys = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(decode_public)
            .collect::<Result<Vec<_>>>()?;
        Ok(Verifier { keys })
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Ok only if some key signed exactly `bytes`. Uses strict verification,
    /// which rejects the malleable and small-order signatures plain Ed25519
    /// tolerates.
    pub fn verify(&self, bytes: &[u8], signature_b64: &str) -> Result<()> {
        if self.keys.is_empty() {
            return Err(Error::NoKeys);
        }
        let raw = B64
            .decode(signature_b64.trim())
            .map_err(|_| Error::BadSignature)?;
        let sig = Signature::from_slice(&raw).map_err(|_| Error::BadSignature)?;
        if self.keys.iter().any(|k| k.verify_strict(bytes, &sig).is_ok()) {
            Ok(())
        } else {
            Err(Error::BadSignature)
        }
    }
}

fn decode_public(line: &str) -> Result<VerifyingKey> {
    let raw: [u8; 32] = B64
        .decode(line)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| Error::Manifest(format!("bad public key {line:?}")))?;
    VerifyingKey::from_bytes(&raw).map_err(|_| Error::Manifest(format!("bad public key {line:?}")))
}

/// The release side: holds the private seed. Only `release sign` builds one.
pub struct Signer {
    key: SigningKey,
}

impl Signer {
    /// A fresh key from the OS random source.
    pub fn generate() -> Result<Signer> {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).map_err(|e| Error::Unsupported(e.to_string()))?;
        Ok(Signer {
            key: SigningKey::from_bytes(&seed),
        })
    }

    pub fn from_seed_b64(seed: &str) -> Result<Signer> {
        let raw: [u8; 32] = B64
            .decode(seed.trim())
            .ok()
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| Error::Unsupported("signing key is not 32 bytes of base64".into()))?;
        Ok(Signer {
            key: SigningKey::from_bytes(&raw),
        })
    }

    pub fn seed_b64(&self) -> String {
        B64.encode(self.key.to_bytes())
    }

    pub fn public_b64(&self) -> String {
        B64.encode(self.key.verifying_key().to_bytes())
    }

    pub fn sign(&self, bytes: &[u8]) -> String {
        B64.encode(self.key.sign(bytes).to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_and_verifies_and_rejects_tampering() {
        let s = Signer::generate().unwrap();
        let v = Verifier::from_lines(&format!("# comment\n{}\n", s.public_b64())).unwrap();
        let sig = s.sign(b"manifest");
        v.verify(b"manifest", &sig).unwrap();
        assert!(matches!(v.verify(b"manifesT", &sig), Err(Error::BadSignature)));
        assert!(matches!(v.verify(b"manifest", "not base64!"), Err(Error::BadSignature)));

        let other = Signer::generate().unwrap();
        assert!(v.verify(b"manifest", &other.sign(b"manifest")).is_err());
    }

    #[test]
    fn seed_round_trips() {
        let s = Signer::generate().unwrap();
        let t = Signer::from_seed_b64(&s.seed_b64()).unwrap();
        assert_eq!(s.public_b64(), t.public_b64());
    }

    #[test]
    fn rotation_accepts_either_key() {
        let (a, b) = (Signer::generate().unwrap(), Signer::generate().unwrap());
        let v = Verifier::from_lines(&format!("{}\n{}", a.public_b64(), b.public_b64())).unwrap();
        v.verify(b"x", &a.sign(b"x")).unwrap();
        v.verify(b"x", &b.sign(b"x")).unwrap();
    }

    #[test]
    fn no_keys_is_reported_not_accepted() {
        let v = Verifier::from_lines("# nothing yet\n").unwrap();
        assert!(matches!(v.verify(b"x", "AAAA"), Err(Error::NoKeys)));
    }

    #[test]
    fn shipped_key_file_parses() {
        Verifier::from_lines(SHIPPED).expect("update-keys.txt has a malformed line");
    }
}
