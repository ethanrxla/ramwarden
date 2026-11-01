//! The signed entitlement record the daemon stores as `entitlement.json`.
//!
//! The daemon unlocks a paid warden by reading a file on its own disk, and
//! anyone with that disk can edit a file. So the record is signed with the
//! server's Ed25519 key and the daemon checks it against the public key
//! compiled into `ramwarden-core` (`wardens::Entitlement::load`). An edited,
//! unsigned or foreign record loads as "nothing unlocked".
//!
//! # What is signed
//!
//! Not the JSON: two JSON encoders can disagree on key order and number
//! formatting, and a verifier would have to re-encode exactly as the signer
//! did. The signature covers this text, which both sides build from the
//! parsed fields, one item per line, joined by `\n`, no trailing newline:
//!
//! ```text
//! ramwarden-entitlement-v1
//! account=<account id>
//! device=<device id>
//! issued_at=<unix seconds>
//! warden:<slug>=<unix seconds>      one line per warden, sorted by slug
//! ```
//!
//! Expiries are absolute, so an old record can only under-state a pass; the
//! daemon refreshes it after a purchase and at least daily.

use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};

pub const DOMAIN: &str = "ramwarden-entitlement-v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub v: u32,
    pub account: String,
    pub device: String,
    pub issued_at: u64,
    pub wardens: BTreeMap<String, u64>,
    pub sig: String,
}

/// The exact bytes signed for these fields.
pub fn signed_text(account: &str, device: &str, issued_at: u64, wardens: &BTreeMap<String, u64>) -> String {
    let mut s = format!("{DOMAIN}\naccount={account}\ndevice={device}\nissued_at={issued_at}");
    for (w, t) in wardens {
        s.push_str(&format!("\nwarden:{w}={t}"));
    }
    s
}

pub struct EntitlementSigner {
    key: SigningKey,
}

impl EntitlementSigner {
    /// From base64 of a 32-byte Ed25519 seed (`RAMWARDEN_ENTITLEMENT_KEY`).
    pub fn from_base64(seed: &str) -> Result<Self, String> {
        let bytes: [u8; 32] = B64
            .decode(seed.trim())
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or("RAMWARDEN_ENTITLEMENT_KEY must be base64 of 32 bytes")?;
        Ok(EntitlementSigner { key: SigningKey::from_bytes(&bytes) })
    }

    /// Base64 of the public key, the value compiled into the daemon.
    pub fn public_key(&self) -> String {
        B64.encode(self.key.verifying_key().as_bytes())
    }

    pub fn sign(&self, account: &str, device: &str, issued_at: u64, wardens: BTreeMap<String, u64>) -> Record {
        let sig = self.key.sign(signed_text(account, device, issued_at, &wardens).as_bytes());
        Record {
            v: 1,
            account: account.into(),
            device: device.into(),
            issued_at,
            wardens,
            sig: B64.encode(sig.to_bytes()),
        }
    }
}

/// What the daemon does: true only for a v1 record signed by `public_key`.
pub fn verify(record: &Record, public_key_b64: &str) -> bool {
    let Some(key) = B64
        .decode(public_key_b64.trim())
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .and_then(|b| VerifyingKey::from_bytes(&b).ok())
    else {
        return false;
    };
    let Some(sig) = B64.decode(&record.sig).ok().and_then(|b| <[u8; 64]>::try_from(b).ok()) else {
        return false;
    };
    record.v == 1
        && key
            .verify(
                signed_text(&record.account, &record.device, record.issued_at, &record.wardens).as_bytes(),
                &ed25519_dalek::Signature::from_bytes(&sig),
            )
            .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed test key, so the vector below can be checked by the daemon's
    /// verifier in `ramwarden-core` too.
    pub const TEST_SEED: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

    #[test]
    fn a_signed_record_verifies_and_any_edit_breaks_it() {
        let s = EntitlementSigner::from_base64(TEST_SEED).unwrap();
        let w = BTreeMap::from([("ram".to_string(), 1_795_000_000), ("wazuh".to_string(), 1_796_000_000)]);
        let r = s.sign("acct1", "dev1", 1_793_000_000, w);
        assert!(verify(&r, &s.public_key()));

        let mut longer = r.clone();
        longer.wardens.insert("wazuh".into(), 4_000_000_000);
        let mut added = r.clone();
        added.wardens.insert("future".into(), 4_000_000_000);
        let mut moved = r.clone();
        moved.device = "dev2".into();
        for bad in [longer, added, moved] {
            assert!(!verify(&bad, &s.public_key()));
        }
        let other = EntitlementSigner::from_base64(&B64.encode([2u8; 32])).unwrap();
        assert!(!verify(&r, &other.public_key()), "another server's key");
    }

    #[test]
    fn the_signed_text_is_stable() {
        let w = BTreeMap::from([("wazuh".to_string(), 2), ("ram".to_string(), 1)]);
        assert_eq!(
            signed_text("a", "d", 9, &w),
            "ramwarden-entitlement-v1\naccount=a\ndevice=d\nissued_at=9\nwarden:ram=1\nwarden:wazuh=2"
        );
    }

    #[test]
    fn the_record_round_trips_as_the_daemon_reads_it() {
        let s = EntitlementSigner::from_base64(TEST_SEED).unwrap();
        let r = s.sign("a", "d", 9, BTreeMap::from([("ram".to_string(), 10)]));
        let json = serde_json::to_string(&r).unwrap();
        let back: Record = serde_json::from_str(&json).unwrap();
        assert!(verify(&back, &s.public_key()));
        assert!(EntitlementSigner::from_base64("short").is_err());
    }
}
