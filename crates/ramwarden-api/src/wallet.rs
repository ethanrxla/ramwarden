//! Wallet sign-in: the SIWE (EIP-4361) and SIWS messages the server words, and
//! the signature checks for MetaMask (`personal_sign`) and Phantom
//! (`signMessage`).
//!
//! # The server words the message, and keeps it
//!
//! `challenge` composes the whole message and stores it under its nonce.
//! `verify` accepts only that exact text back, signed by the address it names.
//! Nothing in the signed text is parsed for meaning except the nonce used to
//! find it, so there is no parser for an attacker to confuse: a message the
//! server did not write, or wrote for another address, domain or device code,
//! is refused before any signature maths runs.

use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey as EcdsaKey};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Chain {
    /// Any EVM chain: MetaMask, and Phantom's EVM accounts. One address works
    /// on all of them.
    Evm,
    Solana,
}

impl Chain {
    /// The address in the one form it is stored and compared in, or why it is
    /// not an address. EVM addresses are case-insensitive hex (EIP-55 case is
    /// only a checksum), so they are lowercased or one wallet would become two
    /// accounts. Solana addresses are base58 of a 32-byte key and keep their
    /// case.
    pub fn normalise(self, address: &str) -> Result<String, String> {
        let a = address.trim();
        match self {
            Chain::Evm => {
                let hex = a.strip_prefix("0x").or_else(|| a.strip_prefix("0X")).unwrap_or("");
                if hex.len() != 40 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("not an Ethereum address".into());
                }
                Ok(format!("0x{}", hex.to_ascii_lowercase()))
            }
            Chain::Solana => match bs58::decode(a).into_vec() {
                Ok(k) if k.len() == 32 => Ok(a.to_string()),
                _ => Err("not a Solana address".into()),
            },
        }
    }

    /// The address as wallets display it: EIP-55 checksummed for EVM.
    pub fn display(self, normalised: &str) -> String {
        match self {
            Chain::Evm => checksum(normalised),
            Chain::Solana => normalised.to_string(),
        }
    }
}

pub fn keccak(data: &[u8]) -> [u8; 32] {
    Keccak256::digest(data).into()
}

/// EIP-55 mixed-case checksum of a lowercase `0x` address.
pub fn checksum(lower: &str) -> String {
    let hex = lower.trim_start_matches("0x");
    let h = keccak(hex.as_bytes());
    let mut out = String::from("0x");
    for (i, c) in hex.chars().enumerate() {
        let nibble = (h[i / 2] >> if i % 2 == 0 { 4 } else { 0 }) & 0x0f;
        out.push(if nibble >= 8 { c.to_ascii_uppercase() } else { c });
    }
    out
}

/// What goes into a sign-in message besides the address.
pub struct MessageParts<'a> {
    /// The site's host, e.g. `ramwarden.app`. Wallets warn when it does not
    /// match the page asking for the signature, which is the phishing guard.
    pub domain: &'a str,
    /// The site's origin, e.g. `https://ramwarden.app`.
    pub uri: &'a str,
    /// EIP-155 chain id named in a SIWE message. Sign-in is chain-agnostic;
    /// this is the chain checkout uses, so the wallet is already there.
    pub chain_id: u64,
    pub nonce: &'a str,
    pub issued_at: u64,
    pub expires_at: u64,
    /// When set, a valid signature also approves this device sign-in.
    pub user_code: Option<&'a str>,
}

const STATEMENT: &str = "Sign in to RamWarden. This does not move funds or approve any transaction.";

pub fn device_resource(user_code: &str) -> String {
    format!("urn:ramwarden:device:{user_code}")
}

/// The full SIWE or SIWS text for `address` (already normalised).
pub fn message(chain: Chain, address: &str, p: &MessageParts) -> String {
    let (who, chain_line) = match chain {
        Chain::Evm => ("Ethereum", format!("Chain ID: {}\n", p.chain_id)),
        Chain::Solana => ("Solana", String::new()),
    };
    let mut m = format!(
        "{domain} wants you to sign in with your {who} account:\n{address}\n\n{STATEMENT}\n\n\
         URI: {uri}\nVersion: 1\n{chain_line}Nonce: {nonce}\nIssued At: {issued}\nExpiration Time: {expires}",
        domain = p.domain,
        address = chain.display(address),
        uri = p.uri,
        nonce = p.nonce,
        issued = iso8601(p.issued_at),
        expires = iso8601(p.expires_at),
    );
    if let Some(code) = p.user_code {
        m.push_str(&format!("\nResources:\n- {}", device_resource(code)));
    }
    m
}

/// The nonce a message quotes, to find what the server stored for it.
pub fn nonce_of(message: &str) -> Option<&str> {
    message.lines().find_map(|l| l.strip_prefix("Nonce: ")).map(str::trim)
}

/// Whether `signature` is `address` signing exactly `message`.
pub fn verify(chain: Chain, address: &str, message: &str, signature: &str) -> Result<(), String> {
    match chain {
        Chain::Evm => verify_evm(address, message, signature),
        Chain::Solana => verify_solana(address, message, signature),
    }
}

/// EIP-191 `personal_sign`: keccak of the prefixed message, signed with the
/// account's secp256k1 key. The signer is recovered and compared by address.
fn verify_evm(address: &str, message: &str, signature: &str) -> Result<(), String> {
    let bad = || "signature is not 65 bytes of hex".to_string();
    let sig = hex::decode(signature.trim().trim_start_matches("0x")).map_err(|_| bad())?;
    if sig.len() != 65 {
        return Err(bad());
    }
    let v = match sig[64] {
        27 | 28 => sig[64] - 27,
        0 | 1 => sig[64],
        _ => return Err("signature has an unknown recovery id".into()),
    };
    let mut s = EcdsaSignature::from_slice(&sig[..64]).map_err(|_| "signature is malformed".to_string())?;
    let mut rec = RecoveryId::from_byte(v).ok_or("signature has an unknown recovery id")?;
    // Wallets sign low-S, but nothing in EIP-191 requires it; flip a high-S
    // signature to its low-S twin, which recovers the same key.
    if let Some(low) = s.normalize_s() {
        s = low;
        rec = RecoveryId::new(!rec.is_y_odd(), rec.is_x_reduced());
    }
    let hash = personal_hash(message);
    let key = EcdsaKey::recover_from_prehash(&hash, &s, rec).map_err(|_| "signature does not verify".to_string())?;
    if evm_address(&key) == Chain::Evm.normalise(address)? {
        Ok(())
    } else {
        Err("signature is from a different wallet".into())
    }
}

pub fn personal_hash(message: &str) -> [u8; 32] {
    let mut data = format!("\x19Ethereum Signed Message:\n{}", message.len()).into_bytes();
    data.extend_from_slice(message.as_bytes());
    keccak(&data)
}

pub fn evm_address(key: &EcdsaKey) -> String {
    let point = key.to_encoded_point(false);
    let h = keccak(&point.as_bytes()[1..]);
    format!("0x{}", hex::encode(&h[12..]))
}

/// Phantom's `signMessage`: a plain Ed25519 signature over the UTF-8 bytes,
/// by the key the address encodes.
fn verify_solana(address: &str, message: &str, signature: &str) -> Result<(), String> {
    let key: [u8; 32] = bs58::decode(address.trim())
        .into_vec()
        .ok()
        .and_then(|k| k.try_into().ok())
        .ok_or("not a Solana address")?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&key).map_err(|_| "not a Solana address".to_string())?;
    let sig: [u8; 64] = bs58::decode(signature.trim())
        .into_vec()
        .ok()
        .and_then(|s| s.try_into().ok())
        .ok_or("signature is not 64 bytes of base58")?;
    key.verify_strict(message.as_bytes(), &ed25519_dalek::Signature::from_bytes(&sig))
        .map_err(|_| "signature does not verify".to_string())
}

/// `YYYY-MM-DDTHH:MM:SSZ` for Unix seconds, as SIWE requires (RFC 3339).
pub fn iso8601(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
}

#[cfg(test)]
pub(crate) mod testing {
    //! Wallets for tests: deterministic keys that sign the way MetaMask and
    //! Phantom do.
    use super::*;
    use ed25519_dalek::Signer;
    use k256::ecdsa::SigningKey;

    pub struct EvmWallet(pub SigningKey);
    impl EvmWallet {
        pub fn new(seed: u8) -> Self {
            EvmWallet(SigningKey::from_slice(&[seed; 32]).unwrap())
        }
        pub fn address(&self) -> String {
            checksum(&evm_address(self.0.verifying_key()))
        }
        pub fn personal_sign(&self, message: &str) -> String {
            let (sig, rec) = self.0.sign_prehash_recoverable(&personal_hash(message)).unwrap();
            let mut bytes = sig.to_bytes().to_vec();
            bytes.push(27 + rec.to_byte());
            format!("0x{}", hex::encode(bytes))
        }
    }

    pub struct SolWallet(pub ed25519_dalek::SigningKey);
    impl SolWallet {
        pub fn new(seed: u8) -> Self {
            SolWallet(ed25519_dalek::SigningKey::from_bytes(&[seed; 32]))
        }
        pub fn address(&self) -> String {
            bs58::encode(self.0.verifying_key().as_bytes()).into_string()
        }
        pub fn sign_message(&self, message: &str) -> String {
            bs58::encode(self.0.sign(message.as_bytes()).to_bytes()).into_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn parts(code: Option<&str>) -> MessageParts<'_> {
        MessageParts {
            domain: "ramwarden.app",
            uri: "https://ramwarden.app",
            chain_id: 8453,
            nonce: "n0nce123",
            issued_at: 1_793_000_000,
            expires_at: 1_793_000_600,
            user_code: code,
        }
    }

    #[test]
    fn eip55_matches_the_spec_vectors() {
        for a in ["0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed", "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359"] {
            assert_eq!(checksum(&a.to_ascii_lowercase()), a);
        }
    }

    #[test]
    fn iso8601_formats_utc() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso8601(1_793_000_000), "2026-10-26T07:33:20Z");
    }

    #[test]
    fn addresses_normalise_or_are_refused() {
        let w = EvmWallet::new(7);
        assert_eq!(Chain::Evm.normalise(&w.address()).unwrap(), w.address().to_ascii_lowercase());
        assert!(Chain::Evm.normalise("0x1234").is_err());
        assert!(Chain::Evm.normalise("1234567890123456789012345678901234567890").is_err());
        let s = SolWallet::new(7);
        assert_eq!(Chain::Solana.normalise(&s.address()).unwrap(), s.address());
        assert!(Chain::Solana.normalise("0OIl").is_err());
    }

    #[test]
    fn the_siwe_message_has_the_eip4361_shape_and_the_device_resource() {
        let w = EvmWallet::new(1);
        let addr = Chain::Evm.normalise(&w.address()).unwrap();
        let m = message(Chain::Evm, &addr, &parts(Some("ABCD-EFGH")));
        let lines: Vec<&str> = m.lines().collect();
        assert_eq!(lines[0], "ramwarden.app wants you to sign in with your Ethereum account:");
        assert_eq!(lines[1], w.address(), "checksummed in the message");
        assert!(m.contains("\nChain ID: 8453\nNonce: n0nce123\nIssued At: 2026-10-26T07:33:20Z\n"));
        assert!(m.ends_with("Resources:\n- urn:ramwarden:device:ABCD-EFGH"));
        assert_eq!(nonce_of(&m), Some("n0nce123"));
        let s = message(Chain::Solana, &SolWallet::new(1).address(), &parts(None));
        assert!(s.starts_with("ramwarden.app wants you to sign in with your Solana account:\n"));
        assert!(!s.contains("Chain ID") && !s.contains("Resources"));
    }

    #[test]
    fn metamask_signatures_verify_for_their_own_address_only() {
        let (w, other) = (EvmWallet::new(1), EvmWallet::new(2));
        let m = message(Chain::Evm, &w.address().to_ascii_lowercase(), &parts(None));
        let sig = w.personal_sign(&m);
        assert_eq!(verify(Chain::Evm, &w.address(), &m, &sig), Ok(()));
        assert_eq!(verify(Chain::Evm, &w.address().to_ascii_lowercase(), &m, &sig), Ok(()), "case-insensitive");
        assert!(verify(Chain::Evm, &other.address(), &m, &sig).is_err());
        assert!(verify(Chain::Evm, &w.address(), &format!("{m} "), &sig).is_err(), "one byte changed");
        assert!(verify(Chain::Evm, &w.address(), &m, "0x1234").is_err());
        // v as 0/1, as some wallets send it.
        let mut raw = hex::decode(&sig[2..]).unwrap();
        raw[64] -= 27;
        assert_eq!(verify(Chain::Evm, &w.address(), &m, &hex::encode(raw)), Ok(()));
    }

    #[test]
    fn phantom_signatures_verify_for_their_own_address_only() {
        let (w, other) = (SolWallet::new(1), SolWallet::new(2));
        let m = message(Chain::Solana, &w.address(), &parts(Some("ABCD-EFGH")));
        let sig = w.sign_message(&m);
        assert_eq!(verify(Chain::Solana, &w.address(), &m, &sig), Ok(()));
        assert!(verify(Chain::Solana, &other.address(), &m, &sig).is_err());
        assert!(verify(Chain::Solana, &w.address(), &m.replace("ABCD", "WXYZ"), &sig).is_err());
        assert!(verify(Chain::Solana, &w.address(), &m, "abc").is_err());
    }
}
