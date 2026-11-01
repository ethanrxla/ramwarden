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
