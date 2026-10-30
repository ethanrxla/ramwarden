//! Who may talk to the daemon.
//!
//! # The threat
//!
//! The daemon listens on a local port with no credentials and exposes actions
//! that freeze and kill applications. Any web page the user visits can make
//! the browser send a request to `http://127.0.0.1:7823`. CORS does not stop
//! that: a cross-site `POST` with a "simple" content type is *sent* without a
//! preflight, and CORS only hides the response. So with a permissive or absent
//! check, `<form action="http://127.0.0.1:7823/kill/brave?force=true">` on any
//! site would close the user's browser.
//!
//! # The rule
//!
//! Browsers always attach an `Origin` header to cross-site requests and to
//! WebSocket handshakes. Non-browser clients — the RamWarden window, `curl`,
//! scripts — send none. So:
//!
//! * a request **with** an `Origin` must come from a browser extension
//!   (`chrome-extension://`, `moz-extension://`) or from the daemon's own
//!   origin; anything else is refused before routing;
//! * a request **without** one is a local program, which already runs as the
//!   user and could do the same things directly.
//!
//! On a loopback bind the `Host` header must also be a loopback name. That
//! closes DNS rebinding, where an attacker's domain re-resolves to 127.0.0.1
//! and the browser considers the request same-origin.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

/// What the guard needs to know about how the daemon is exposed.
#[derive(Clone, Debug)]
pub struct Policy {
    /// The daemon is bound to loopback only, so `Host` must be loopback too.
    pub loopback: bool,
    pub port: u16,
}

impl Policy {
    pub fn for_bind(host: &str, port: u16) -> Self {
        Policy {
            loopback: is_loopback(host),
            port,
        }
    }

    /// May a request carrying this `Origin` be served?
    pub fn origin_allowed(&self, origin: &str) -> bool {
        if origin.starts_with("chrome-extension://")
            || origin.starts_with("moz-extension://")
            || origin.starts_with("safari-web-extension://")
        {
            return true;
        }
        // The daemon's own origin, for a page it serves itself.
        let Some(rest) = origin.strip_prefix("http://") else {
            return false;
        };
        let (host, port) = split_host_port(rest);
        is_loopback(host) && port == Some(self.port)
    }

    /// May a request addressed to this `Host` be served?
    pub fn host_allowed(&self, host: Option<&str>) -> bool {
        if !self.loopback {
            return true;
        }
        match host {
            // HTTP/1.1 requires Host; a client that omits it is not a browser.
            None => true,
            Some(h) => is_loopback(split_host_port(h).0),
        }
    }

    /// The full check, as a status to refuse with.
    pub fn check(&self, headers: &HeaderMap) -> Result<(), &'static str> {
        let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
        if !self.host_allowed(host) {
            return Err("Host is not a loopback name (possible DNS rebinding)");
        }
        if let Some(origin) = headers.get(header::ORIGIN) {
            let ok = origin.to_str().map(|o| self.origin_allowed(o)).unwrap_or(false);
            if !ok {
                return Err("requests from web pages are not accepted");
            }
        }
        Ok(())
    }

    /// CORS for the allowed origins only, so an extension can read replies.
    pub fn cors(&self) -> CorsLayer {
        let me = self.clone();
        CorsLayer::new()
            .allow_origin(AllowOrigin::predicate(move |origin: &HeaderValue, _| {
                origin.to_str().map(|o| me.origin_allowed(o)).unwrap_or(false)
            }))
            .allow_methods(Any)
            .allow_headers(Any)
    }
}

/// Middleware: refuse anything the [`Policy`] rejects, before any handler runs.
pub async fn enforce(State(policy): State<Policy>, req: Request, next: Next) -> Response {
    match policy.check(req.headers()) {
        Ok(()) => next.run(req).await,
        Err(why) => {
            tracing::warn!(
                "refused {} {} — {why} (origin {:?})",
                req.method(),
                req.uri().path(),
                req.headers().get(header::ORIGIN)
            );
            (StatusCode::FORBIDDEN, why).into_response()
        }
    }
}

pub fn is_loopback(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Split `host:port`, tolerating bracketed IPv6.
fn split_host_port(s: &str) -> (&str, Option<u16>) {
    let s = s.split('/').next().unwrap_or(s);
    if let Some(rest) = s.strip_prefix('[') {
        let (h, tail) = rest.split_once(']').unwrap_or((rest, ""));
        return (h, tail.strip_prefix(':').and_then(|p| p.parse().ok()));
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h, p.parse().ok()),
        _ => (s, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local() -> Policy {
        Policy::for_bind("127.0.0.1", 7823)
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn extensions_and_local_programs_are_accepted() {
        let p = local();
        assert!(p.check(&headers(&[("host", "127.0.0.1:7823")])).is_ok(), "curl / the window");
        assert!(p.check(&headers(&[("host", "localhost:7823"), ("origin", "chrome-extension://abcdef")])).is_ok());
        assert!(p.check(&headers(&[("host", "127.0.0.1:7823"), ("origin", "moz-extension://1234-5678")])).is_ok());
        assert!(p.check(&headers(&[("host", "[::1]:7823"), ("origin", "http://127.0.0.1:7823")])).is_ok());
    }

    /// The attack this module exists for.
    #[test]
    fn a_web_page_cannot_reach_a_destructive_route() {
        let p = local();
        for origin in ["https://evil.example", "http://localhost:3000", "null", "http://127.0.0.1:9999"] {
            assert!(
                p.check(&headers(&[("host", "127.0.0.1:7823"), ("origin", origin)])).is_err(),
                "{origin} must be refused"
            );
        }
    }

    #[test]
    fn dns_rebinding_is_refused_on_a_loopback_bind() {
        let p = local();
        assert!(p.check(&headers(&[("host", "attacker.example:7823")])).is_err());
        assert!(p.check(&headers(&[("host", "127.0.0.1.nip.io:7823")])).is_err());
    }

    #[test]
    fn a_deliberate_network_bind_skips_the_host_check_but_not_the_origin_check() {
        let p = Policy::for_bind("0.0.0.0", 7823);
        assert!(!p.loopback);
        assert!(p.check(&headers(&[("host", "100.64.0.7:7823")])).is_ok());
        assert!(p.check(&headers(&[("host", "100.64.0.7:7823"), ("origin", "https://evil.example")])).is_err());
    }

    #[test]
    fn loopback_names_are_recognised() {
        for h in ["127.0.0.1", "localhost", "LOCALHOST", "::1", "[::1]", "127.4.5.6"] {
            assert!(is_loopback(h), "{h}");
        }
        for h in ["0.0.0.0", "192.168.1.5", "example.com", "localhost.example.com"] {
            assert!(!is_loopback(h), "{h}");
        }
    }
}
