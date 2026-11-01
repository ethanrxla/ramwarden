//! Downloads, behind a trait so tests never touch the network.

use std::io::{Read, Write};
use std::time::Duration;

use crate::{Error, Result};

/// The manifest is a few kilobytes; anything near this is not a manifest.
pub const MANIFEST_LIMIT: u64 = 1 << 20;

pub trait Fetch {
    /// Stream `url` into `sink`, stopping with an error past `limit` bytes.
    /// `bearer` is sent as `Authorization: Bearer …` (paid downloads only).
    fn fetch(&self, url: &str, bearer: Option<&str>, limit: u64, sink: &mut dyn Write) -> Result<u64>;

    /// Whole body as bytes; for the manifest and its signature.
    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.fetch(url, None, limit, &mut out)?;
        Ok(out)
    }

    /// True if a loopback health URL answers 2xx within a second.
    fn healthy(&self, url: &str) -> bool;

    /// POST a JSON body; the status and body come back whatever the status,
    /// because the caller reads the API's error codes. Err only when no
    /// answer arrived at all.
    fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<(u16, Vec<u8>)>;
}

/// HTTPS through `ureq` with rustls, so no OpenSSL on any platform.
pub struct Http {
    agent: ureq::Agent,
}

impl Default for Http {
    fn default() -> Self {
        Http {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout_read(Duration::from_secs(60))
                .user_agent(concat!("ramwarden-launcher/", env!("CARGO_PKG_VERSION")))
                .build(),
        }
    }
}

impl Fetch for Http {
    fn fetch(&self, url: &str, bearer: Option<&str>, limit: u64, sink: &mut dyn Write) -> Result<u64> {
        // A redirect may go anywhere (GitHub sends assets to its CDN), but the
        // request must start on HTTPS: a bearer token never goes out in clear.
        if !url.starts_with("https://") {
            return Err(Error::Fetch(format!("{url} is not HTTPS")));
        }
        let mut req = self.agent.get(url);
        if let Some(token) = bearer {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        let resp = req.call().map_err(|e| match e {
            ureq::Error::Status(401 | 403, _) => Error::NeedsPro(url.to_string()),
            other => Error::Fetch(format!("{url}: {other}")),
        })?;
        let mut body = resp.into_reader().take(limit + 1);
        let n = std::io::copy(&mut body, sink)?;
        if n > limit {
            return Err(Error::Fetch(format!("{url}: larger than {limit} bytes")));
        }
        Ok(n)
    }

    fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<(u16, Vec<u8>)> {
        if !url.starts_with("https://") {
            return Err(Error::Fetch(format!("{url} is not HTTPS")));
        }
        let bytes = serde_json::to_vec(body).map_err(|e| Error::Fetch(e.to_string()))?;
        let req = self.agent.post(url).set("Content-Type", "application/json");
        let resp = match req.send_bytes(&bytes) {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(Error::Fetch(format!("{url}: {e}"))),
        };
        let status = resp.status();
        let mut out = Vec::new();
        resp.into_reader().take(64 * 1024).read_to_end(&mut out)?;
        Ok((status, out))
    }

    fn healthy(&self, url: &str) -> bool {
        if !crate::manifest::is_loopback_url(url) {
            return false;
        }
        self.agent
            .get(url)
            .timeout(Duration::from_secs(1))
            .call()
            .is_ok()
    }
}
