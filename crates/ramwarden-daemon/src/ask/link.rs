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

/// The http(s) links in a message, in order, without duplicates.
pub fn urls(message: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in message.split_whitespace() {
        let start = word.find("http://").or_else(|| word.find("https://"));
        let Some(start) = start else { continue };
        let raw = word[start..].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '>', '"', '\'']);
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

/// Fetch one link and reduce it to a small reading.
pub async fn read(link: &str) -> Value {
    let client = match reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 || !matches!(attempt.url().scheme(), "http" | "https") {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .user_agent(concat!("RamWarden/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        Err(_) => return failed(link, "the request could not be set up"),
    };
    let mut response = match client.get(link).send().await {
        Ok(r) => r,
        Err(e) => return failed(link, why(&e, link)),
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
        v["title"] = json!(t);
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
    } else if e.is_redirect() {
        "it redirected too many times".into()
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
    }

    #[test]
    fn a_page_is_reduced_to_its_title_and_visible_text() {
        let html = "<html><head><title>Agent &amp; memory</title><style>p{}</style></head>\
                    <body><script>var x = '<p>hidden</p>';</script><h1>Graph</h1><p>3 nodes,&nbsp;2 edges</p></body></html>";
        let (title, text) = page_text(html);
        assert_eq!(title.as_deref(), Some("Agent & memory"));
        assert_eq!(text, "Graph 3 nodes, 2 edges");
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
        assert_eq!(v["text"], r#"{"edges":2,"nodes":3}"#);
        // Port 9 on loopback: nothing listens there.
        let v = read("http://127.0.0.1:9/").await;
        assert_eq!(v["read"], false);
        assert!(v["note"].as_str().unwrap().contains("could not connect"), "{v}");
    }
}
