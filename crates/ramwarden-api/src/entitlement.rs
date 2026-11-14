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
//! ramwarden-entitlement-v2
//! account=<account id>
//! device=<device id>
//! issued_at=<unix seconds>
//! tier=<ram | pro | none>
//! status=<trial | active | grace | lapsed | cancelled | none>
//! warden:<slug>=<unix seconds>      one line per warden, sorted by slug
//! ```
//!
//! `tier` and `status` are there for the menu bar and ThreatWarden's
//! installer, which show "trial ends in 3 days" or "lapsed: local mode"
//! without asking the server. They never widen access: what unlocks a
//! warden is still its expiry line. Expiries are absolute and already
//! include the grace period, so an old record can only under-state access;
//! the daemon refreshes it after a checkout and at least daily.
//!
//! v1 (warden lines only) never shipped, so the verifier accepts v2 alone.

use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};

pub const DOMAIN: &str = "ramwarden-entitlement-v2";
pub const VERSION: u32 = 2;
/// What `tier` and `status` read when nothing has been bought.
pub const NONE: &str = "none";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub v: u32,
    pub account: String,
    pub device: String,
    pub issued_at: u64,
    pub tier: String,
    pub status: String,
    pub wardens: BTreeMap<String, u64>,
    pub sig: String,
}

/// The fields a record signs, before signing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claims {
    pub account: String,
    pub device: String,
    pub issued_at: u64,
    pub tier: String,
    pub status: String,
    pub wardens: BTreeMap<String, u64>,
}

impl Claims {
    /// What an account is entitled to at `now`, for one of its devices.
    pub fn for_account(a: &ramwarden_cloud::model::Account, device: &str, now: u64) -> Self {
        Claims {
            account: a.id.clone(),
            device: device.into(),
            issued_at: now,
            tier: a.tier.map_or(NONE, |t| t.as_str()).into(),
            status: a.status(now).map_or(NONE, |s| s.as_str()).into(),
            wardens: a.wardens(),
        }
    }

    /// The exact bytes signed.
    pub fn signed_text(&self) -> String {
        let mut s = format!(
            "{DOMAIN}\naccount={}\ndevice={}\nissued_at={}\ntier={}\nstatus={}",
            self.account, self.device, self.issued_at, self.tier, self.status
        );
        for (w, t) in &self.wardens {
            s.push_str(&format!("\nwarden:{w}={t}"));
        }
        s
    }
}

impl Record {
    pub fn claims(&self) -> Claims {
        Claims {
            account: self.account.clone(),
            device: self.device.clone(),
            issued_at: self.issued_at,
            tier: self.tier.clone(),
            status: self.status.clone(),
            wardens: self.wardens.clone(),
        }
    }
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

    pub fn sign(&self, c: Claims) -> Record {
        let sig = self.key.sign(c.signed_text().as_bytes());
        Record {
            v: VERSION,
            account: c.account,
            device: c.device,
            issued_at: c.issued_at,
            tier: c.tier,
            status: c.status,
            wardens: c.wardens,
            sig: B64.encode(sig.to_bytes()),
        }
    }
}

/// What the daemon does: true only for a v2 record signed by `public_key`.
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
    record.v == VERSION
        && key
            .verify(
                record.claims().signed_text().as_bytes(),
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

    fn claims(wardens: &[(&str, u64)]) -> Claims {
        Claims {
            account: "acct1".into(),
            device: "dev1".into(),
            issued_at: 1_793_000_000,
            tier: "pro".into(),
            status: "trial".into(),
            wardens: wardens.iter().map(|(w, t)| (w.to_string(), *t)).collect(),
        }
    }

    #[test]
    fn a_signed_record_verifies_and_any_edit_breaks_it() {
        let s = EntitlementSigner::from_base64(TEST_SEED).unwrap();
        let r = s.sign(claims(&[("ram", 1_795_000_000), ("threat", 1_796_000_000)]));
        assert!(verify(&r, &s.public_key()));

        let mut longer = r.clone();
        longer.wardens.insert("threat".into(), 4_000_000_000);
        let mut added = r.clone();
        added.wardens.insert("future".into(), 4_000_000_000);
        let mut moved = r.clone();
        moved.device = "dev2".into();
        let mut status = r.clone();
        status.status = "active".into();
        let mut tier = r.clone();
        tier.tier = "ram".into();
        let mut old = r.clone();
        old.v = 1;
        for bad in [longer, added, moved, status, tier, old] {
            assert!(!verify(&bad, &s.public_key()), "{bad:?}");
        }
        let other = EntitlementSigner::from_base64(&B64.encode([2u8; 32])).unwrap();
        assert!(!verify(&r, &other.public_key()), "another server's key");
    }

    #[test]
    fn the_signed_text_is_stable() {
        let mut c = claims(&[("threat", 2), ("ram", 1)]);
        c.account = "a".into();
        c.device = "d".into();
        c.issued_at = 9;
        assert_eq!(
            c.signed_text(),
            "ramwarden-entitlement-v2\naccount=a\ndevice=d\nissued_at=9\ntier=pro\nstatus=trial\nwarden:ram=1\nwarden:threat=2"
        );
    }

    #[test]
    fn claims_follow_the_account() {
        use ramwarden_cloud::model::{Account, BillingUpdate, Identity, Tier};
        let mut a = Account::new("a".into(), Identity::email("x@y.z"), 100);
        let c = Claims::for_account(&a, "d", 100);
        assert_eq!((c.tier.as_str(), c.status.as_str(), c.wardens.len()), ("none", "none", 0));
        BillingUpdate { tier: Some(Tier::Ram), paid_until: Some(1000), trial_ends_at: Some(1000), ..Default::default() }
            .apply(&mut a);
        let c = Claims::for_account(&a, "d", 100);
        assert_eq!((c.tier.as_str(), c.status.as_str()), ("ram", "trial"));
        assert!(c.wardens.contains_key("ram") && !c.wardens.contains_key("threat"));
    }

    #[test]
    fn the_record_round_trips_as_the_daemon_reads_it() {
        let s = EntitlementSigner::from_base64(TEST_SEED).unwrap();
        let r = s.sign(claims(&[("ram", 10)]));
        let json = serde_json::to_string(&r).unwrap();
        let back: Record = serde_json::from_str(&json).unwrap();
        assert!(verify(&back, &s.public_key()));
        assert!(EntitlementSigner::from_base64("short").is_err());
    }
}
