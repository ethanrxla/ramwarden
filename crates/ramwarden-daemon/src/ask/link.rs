//! Read a link the user pasted into the chat.
//!
//! "Can you access http://rxla-pc-1.<tailnet>.ts.net:8787/graph?" used to get
//! "that's outside my read-only probes" (2026-11-18): the warden had no way
//! to open a page, even one served on the user's own machine or tailnet.
//!
//! The rule that keeps this safe: only a URL the **user** typed in this
//! message is read. The model cannot ask for a link ([`super::Probe::from_request`]
//! has no `link`), and text inside a fetched page is never followed, so a
//! page cannot steer the warden to another address. One plain `GET`, at
//! most [`MAX_LINKS`] links, [`TIMEOUT`] each, the first [`MAX_BODY`] bytes,
//! redirects only within http(s). Nothing is posted and no cookies or keys
//! are sent.
//!
//! The page goes to the chat's model like any other reading. The user sent
//! the link for exactly that, so it is not redacted further.

use std::time::Duration;

use serde_json::{Value, json};

/// Links read per message.
pub const MAX_LINKS: usize = 2;
const TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes of a page read before it is cut off.
const MAX_BODY: usize = 512 * 1024;
/// Characters of a page's text passed on. Keeps the reading under the
/// probe cap with room for its other fields.
const MAX_TEXT: usize = 1400;
/// Characters of the link itself shown in a reading.
const MAX_URL: usize = 200;
/// Characters of a page's title passed on.
const MAX_TITLE: usize = 160;
/// Bytes of JSON one link reading may take, under the probe cap.
const BUDGET: usize = super::probes::CAP - 64;

/// The http(s) links in a message, in order, without duplicates.
pub fn urls(message: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in message.split_whitespace() {
        // The earliest scheme in the word: an https link whose query holds
        // `http://...` is the https link.
        let Some(start) = [word.find("http://"), word.find("https://")].into_iter().flatten().min() else { continue };
        let raw = trim_trailing(&word[start..]);
        let Ok(parsed) = url::Url::parse(raw) else { continue };
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() || !parsed.username().is_empty() || parsed.password().is_some() {
            continue;
        }
        let s = parsed.to_string();
        if !out.contains(&s) {
            out.push(s);
        }
        if out.len() == MAX_LINKS {
            break;
        }
    }
    out
}

/// Drop punctuation that ends the sentence rather than the link. A `)` or
/// `]` stays when the link opened one itself (`/wiki/Rust_(language)`).
fn trim_trailing(mut raw: &str) -> &str {
    loop {
        let Some(last) = raw.chars().last() else { return raw };
        let strip = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '>' | '"' | '\'' => true,
            ')' => raw.matches('(').count() < raw.matches(')').count(),
            ']' => raw.matches('[').count() < raw.matches(']').count(),
            _ => false,
        };
        if !strip {
            return raw;
        }
        raw = &raw[..raw.len() - last.len_utf8()];
    }
}

/// The host, for the "Looked at" line.
pub fn host(link: &str) -> String {
    url::Url::parse(link)
        .ok()
        .and_then(|u| u.host_str().map(|h| match u.port() {
            Some(p) => format!("{h}:{p}"),
            None => h.to_string(),
        }))
        .unwrap_or_else(|| "a link".into())
}

/// Fetch one link and reduce it to a small reading, within one
/// [`TIMEOUT`] for the whole link: redirects and body included.
pub async fn read(link: &str) -> Value {
    match tokio::time::timeout(TIMEOUT, fetch(link)).await {
        Ok(v) => fit(v, BUDGET),
        Err(_) => failed(link, "it did not answer within 10 seconds"),
    }
}

async fn fetch(link: &str) -> Value {
    let client = match reqwest::Client::builder()
        .timeout(TIMEOUT)
        // Redirects are followed only on the host the user typed. A public
        // page that redirects to a private address (a router, a cloud
        // metadata service) is not read on the user's behalf: the reading
        // names where it points instead, and the user can paste that.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("RamWarden/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        Err(_) => return failed(link, "the request could not be set up"),
    };
    let Ok(start) = url::Url::parse(link) else { return failed(link, "it is not a valid link") };
    let mut at = start.clone();
    let mut hops = 0;
    let mut response = loop {
        let r = match client.get(at.clone()).send().await {
            Ok(r) => r,
            Err(e) => return failed(link, why(&e, link)),
        };
        if !r.status().is_redirection() {
            break r;
        }
        let next = r
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|l| at.join(l).ok());
        let Some(next) = next else { break r };
        hops += 1;
        if hops > 5 {
            return failed(link, "it redirected too many times");
        }
        if !same_site(&start, &next) {
            let mut v = failed(link, format!("it redirects to another address ({}); paste that link if you want it read", host(next.as_str())));
            v["status"] = json!(r.status().as_u16());
            return v;
        }
        at = next;
    };
    let status = response.status();
    let kind = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut body: Vec<u8> = Vec::new();
    let mut cut = false;
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let room = MAX_BODY - body.len();
                body.extend_from_slice(&chunk[..chunk.len().min(room)]);
                if body.len() >= MAX_BODY {
                    cut = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => return failed(link, why(&e, link)),
        }
    }
    let text = String::from_utf8_lossy(&body);
    let (title, content) = if kind.contains("json") {
        let compact = serde_json::from_str::<Value>(&text).map(|v| v.to_string()).unwrap_or_else(|_| text.to_string());
        (None, compact)
    } else if kind.contains("html") || text.trim_start().starts_with('<') {
        page_text(&text)
    } else {
        (None, collapse(&text))
    };
    let mut v = json!({
        "probe": "link",
        "url": link,
        "status": status.as_u16(),
        "content_type": kind.split(';').next().unwrap_or("").trim(),
        "text": clip(&content, MAX_TEXT),
    });
    if let Some(t) = title {
        v["title"] = json!(clip(&t, MAX_TITLE));
    }
    if cut || content.chars().count() > MAX_TEXT {
        v["partial"] = json!(true);
    }
    if !status.is_success() {
        v["note"] = json!(format!("the server answered {status}"));
    } else if content.trim().is_empty() {
        v["note"] = json!("the page has no readable text; it is probably drawn by JavaScript. An API address on the same server (often /api/... or a .json link) would show its data");
    }
    v
}

/// Shorten the reading until it fits `budget` bytes of JSON. The URL is
/// bounded first, so a long link never pushes out the page's text; then the
/// text gives way. The shared cap only trims lists, so a link reading
/// budgets itself.
fn fit(mut v: Value, budget: usize) -> Value {
    let size = |v: &Value| serde_json::to_string(v).map_or(0, |s| s.len());
    if let Some(u) = v["url"].as_str().filter(|u| u.chars().count() > MAX_URL) {
        v["url"] = json!(clip(u, MAX_URL));
    }
    while size(&v) > budget {
        let text = v["text"].as_str().unwrap_or("").trim_end_matches('…');
        let n = text.chars().count();
        if n == 0 {
            break;
        }
        v["text"] = json!(clip(text, n * 4 / 5));
        v["partial"] = json!(true);
    }
    v
}

/// Same scheme family and host: http to https on one host is fine.
fn same_site(a: &url::Url, b: &url::Url) -> bool {
    matches!(b.scheme(), "http" | "https") && a.host_str() == b.host_str()
}

fn failed(link: &str, reason: impl Into<String>) -> Value {
    json!({"probe": "link", "url": link, "read": false, "note": reason.into()})
}

/// A plain reason a link could not be read.
fn why(e: &reqwest::Error, link: &str) -> String {
    let tailnet = host(link).contains(".ts.net") || host(link).starts_with("100.");
    if e.is_timeout() {
        "it did not answer within 10 seconds".into()
    } else if e.is_connect() {
        if tailnet {
            "could not connect: check Tailscale is running on this computer and the server on that port is up".into()
        } else {
            "could not connect: the address did not resolve or nothing is listening there".into()
        }
    } else {
        "the page could not be read".into()
    }
}

/// The title and visible text of an HTML page: scripts, styles and tags
/// removed, entities for the common characters decoded, whitespace collapsed.
pub fn page_text(html: &str) -> (Option<String>, String) {
    let lower = html.to_ascii_lowercase();
    let title = lower.find("<title").and_then(|s| {
        let open = s + lower[s..].find('>')? + 1;
        let close = open + lower[open..].find("</title")?;
        Some(collapse(&decode(&html[open..close]))).filter(|t| !t.is_empty())
    });
    let mut out = String::with_capacity(html.len().min(64 * 1024));
    let mut i = 0;
    while i < html.len() {
        let rest = &lower[i..];
        if rest.starts_with('<') {
            let skip_to = ["script", "style", "noscript", "svg", "head"].iter().find_map(|tag| {
                let open = format!("<{tag}");
                let after = rest.as_bytes().get(open.len()).copied();
                (rest.starts_with(&open) && matches!(after, Some(b'>' | b' ' | b'\t' | b'\n' | b'\r' | b'/')))
                    .then(|| rest.find(&format!("</{tag}")).map(|e| i + e))
                    .flatten()
            });
            let from = skip_to.unwrap_or(i);
            match lower[from..].find('>') {
                Some(end) => {
                    out.push(' ');
                    i = from + end + 1;
                }
                None => break,
            }
            continue;
        }
        let next = rest.find('<').map_or(html.len(), |n| i + n);
        out.push_str(&html[i..next]);
        i = next;
    }
    (title, collapse(&decode(&out)))
}

fn decode(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_taken_from_the_message_as_typed() {
        let m = "can you access agent memory at http://rxla-pc-1.tailf11891.ts.net:8787/graph? also (https://example.com/a), http://x.test/ http://x.test/";
        assert_eq!(
            urls(m),
            vec!["http://rxla-pc-1.tailf11891.ts.net:8787/graph".to_string(), "https://example.com/a".to_string()]
        );
        assert!(urls("ftp://x.test file:///etc/passwd javascript:alert(1)").is_empty());
        assert!(urls("http://user:pw@x.test/").is_empty(), "no credentials in links");
        assert_eq!(host("http://rxla-pc-1.tailf11891.ts.net:8787/graph"), "rxla-pc-1.tailf11891.ts.net:8787");
        assert_eq!(urls("see https://en.wikipedia.org/wiki/Rust_(programming_language)."), vec!["https://en.wikipedia.org/wiki/Rust_(programming_language)".to_string()]);
        assert_eq!(urls("(see https://example.com/a)"), vec!["https://example.com/a".to_string()]);
        assert_eq!(urls("https://example.com/r?to=http://other.example/p"), vec!["https://example.com/r?to=http://other.example/p".to_string()]);
    }

    #[test]
    fn a_page_is_reduced_to_its_title_and_visible_text() {
        let html = "<html><head><title>Agent &amp; memory</title><style>p{}</style></head>\
                    <body><script>var x = '<p>hidden</p>';</script><h1>Graph</h1><p>3 nodes,&nbsp;2 edges</p></body></html>";
        let (title, text) = page_text(html);
        assert_eq!(title.as_deref(), Some("Agent & memory"));
        assert_eq!(text, "Graph 3 nodes, 2 edges");
    }

    #[test]
    fn a_reading_fits_its_budget_whatever_the_page() {
        let v = json!({"probe":"link","url":"http://x.test/","status":200,"title":clip(&"t".repeat(10_000), MAX_TITLE),"text":clip(&"é漢".repeat(2_000), MAX_TEXT)});
        let v = fit(v, BUDGET);
        assert!(serde_json::to_string(&v).unwrap().len() <= BUDGET);
        assert_eq!(v["partial"], true);
        // A link longer than the whole budget is shortened, not the text.
        let long = format!("http://x.test/?q={}", "a".repeat(3_000));
        let v = fit(json!({"probe":"link","url":long,"status":200,"text":r#"{"edges":2,"nodes":3}"#}), BUDGET);
        assert_eq!(v["text"], r#"{"edges":2,"nodes":3}"#);
        assert!(serde_json::to_string(&v).unwrap().len() <= BUDGET);
    }

    /// Serve one canned response on a fresh loopback port.
    async fn serve(reply: String) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf).await;
            let _ = s.write_all(reply.as_bytes()).await;
        });
        addr
    }

    #[tokio::test]
    async fn a_redirect_to_another_host_is_named_not_followed() {
        let addr = serve("HTTP/1.1 302 Found\r\nlocation: http://169.254.169.254/latest/meta-data\r\ncontent-length: 0\r\n\r\n".into()).await;
        let v = read(&format!("http://{addr}/go")).await;
        assert_eq!(v["read"], false);
        assert!(v["note"].as_str().unwrap().contains("169.254.169.254"), "{v}");
        assert!(v.get("text").is_none());
    }

    #[tokio::test]
    async fn a_local_server_is_read_and_a_dead_one_is_explained() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf).await;
            let body = r#"{"nodes":3,"edges":2}"#;
            let _ = s
                .write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}", body.len()).as_bytes())
                .await;
        });
        let v = read(&format!("http://{addr}/graph.json")).await;
        assert_eq!(v["status"], 200);
        // Key order depends on serde_json's preserve_order feature, which
        // differs by platform; compare the values.
        let body: Value = serde_json::from_str(v["text"].as_str().unwrap()).unwrap();
        assert_eq!(body, json!({"nodes":3,"edges":2}));
        // Port 9 on loopback: nothing listens there.
        let v = read("http://127.0.0.1:9/").await;
        assert_eq!(v["read"], false);
        assert!(v["note"].as_str().unwrap().contains("could not connect"), "{v}");
    }
}
