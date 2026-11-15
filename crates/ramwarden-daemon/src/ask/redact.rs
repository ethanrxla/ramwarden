//! Command lines with the personal parts taken out.
//!
//! The user decided (2026-11-14) that cloud models may see redacted command
//! lines when answering questions about the computer, because a flag like
//! `--model qwen3-8b.gguf --ctx-size 8192` is often the whole answer to "what
//! is llama-server doing?". What may not leave the machine: home paths,
//! secrets, tokens, URLs with credentials or queries, and email addresses.
//!
//! The rule is to keep the shape and drop the content:
//! - a path keeps only its last component (`C:\Users\ethan\models\q.gguf` → `q.gguf`);
//! - the value of any flag whose name mentions a secret is `[redacted]`;
//! - anything that looks like a credential on its own (long mixed tokens,
//!   `sk-…`, `ghp_…`, JWTs) is `[redacted]`;
//! - a URL keeps only its scheme, host and port;
//! - the result is capped, so a Chromium command line cannot fill the prompt.

/// At most this many arguments survive, and at most this many characters.
const MAX_ARGS: usize = 24;
const MAX_CHARS: usize = 320;
const REDACTED: &str = "[redacted]";

/// Flag names whose value is a secret whatever it looks like.
const SECRET_FLAGS: &[&str] = &[
    "key", "token", "secret", "password", "passwd", "pwd", "auth", "cookie", "credential", "session",
    "bearer", "signature", "private",
];

/// Split a command line the way a shell would for the common cases: spaces
/// separate, double quotes group. Windows command lines quote paths with
/// spaces (`"C:\Program Files\..."`), and those must stay one argument.
fn split(cmdline: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in cmdline.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            '\0' => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

fn is_secret_flag(flag: &str) -> bool {
    let name = flag.trim_start_matches('-').to_ascii_lowercase();
    !name.is_empty() && SECRET_FLAGS.iter().any(|s| name.contains(s))
}

/// Whether a bare value looks like a credential.
fn looks_secret(value: &str) -> bool {
    let v = value.trim();
    if ["sk-", "sk_", "ghp_", "gho_", "github_pat_", "xox", "AKIA", "AIza", "eyJ", "pk_", "rk_", "nvapi-", "hf_"]
        .iter()
        .any(|p| v.starts_with(p) && v.len() >= 12)
    {
        return true;
    }
    // A long run of letters and digits with both in it is a token, not a word.
    let tokenish = v.chars().all(|c| c.is_ascii_alphanumeric() || "-_=+/.".contains(c));
    let digits = v.chars().filter(char::is_ascii_digit).count();
    let letters = v.chars().filter(char::is_ascii_alphabetic).count();
    tokenish && v.len() >= 24 && digits >= 4 && letters >= 4 && !v.contains('.')
}

fn looks_email(value: &str) -> bool {
    let Some((user, host)) = value.split_once('@') else { return false };
    !user.is_empty() && host.contains('.') && !host.contains('/')
}

/// The last component of a path, or `None` when `value` is not a path.
fn path_tail(value: &str) -> Option<&str> {
    let is_path = value.contains('/') || value.contains('\\');
    if !is_path {
        return None;
    }
    value.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().filter(|s| !s.is_empty())
}

/// `scheme://host:port` only: no user info, path, query or fragment.
fn url_origin(value: &str) -> Option<String> {
    let (scheme, rest) = value.split_once("://")?;
    if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+') {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    Some(format!("{scheme}://{host}"))
}

/// One value, with paths shortened and anything personal removed.
fn clean_value(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if let Some(origin) = url_origin(value) {
        return origin;
    }
    if looks_secret(value) || looks_email(value) {
        return REDACTED.into();
    }
    match path_tail(value) {
        Some(tail) if looks_secret(tail) => REDACTED.into(),
        Some(tail) => tail.to_string(),
        None => value.to_string(),
    }
}

/// The command line with personal parts removed. See the module docs.
pub fn command_line(cmdline: &str) -> String {
    let args = split(cmdline);
    let mut out: Vec<String> = Vec::with_capacity(args.len().min(MAX_ARGS));
    let mut hide_next = false;
    for (i, arg) in args.iter().enumerate() {
        if out.len() >= MAX_ARGS {
            out.push(format!("… {} more", args.len() - i));
            break;
        }
        if hide_next {
            hide_next = false;
            if !arg.starts_with('-') {
                out.push(REDACTED.into());
                continue;
            }
        }
        if i == 0 {
            out.push(path_tail(arg).unwrap_or(arg).to_string());
            continue;
        }
        if arg.starts_with('-') {
            if let Some((flag, value)) = arg.split_once('=') {
                let value = if is_secret_flag(flag) { REDACTED.into() } else { clean_value(value) };
                out.push(format!("{flag}={value}"));
            } else {
                hide_next = is_secret_flag(arg);
                out.push(arg.clone());
            }
            continue;
        }
        out.push(clean_value(arg));
    }
    let mut text = out.join(" ");
    if text.chars().count() > MAX_CHARS {
        text = text.chars().take(MAX_CHARS).collect::<String>() + "…";
    }
    text
}

#[cfg(test)]
mod tests {
    use super::command_line;

    #[test]
    fn paths_keep_only_their_last_part() {
        let line = r#""C:\Users\ethan\llama.cpp\llama-server.exe" -m "C:\Users\ethan\models\Qwen3 8B-Q4_K_M.gguf" --ctx-size 8192 --port 8080"#;
        assert_eq!(command_line(line), "llama-server.exe -m Qwen3 8B-Q4_K_M.gguf --ctx-size 8192 --port 8080");
        assert_eq!(command_line("/home/ethan/.local/bin/ollama serve"), "ollama serve");
    }

    #[test]
    fn secrets_never_survive() {
        let line = "server --api-key sk-abcdefghijklmnop1234 --token=abc --hf-token hf_ABCDEFGH12345678 --port 9000";
        let out = command_line(line);
        assert!(!out.contains("sk-"), "{out}");
        assert!(!out.contains("abc "), "{out}");
        assert!(!out.contains("hf_"), "{out}");
        assert_eq!(out, "server --api-key [redacted] --token=[redacted] --hf-token [redacted] --port 9000");
        // A bare token with no flag in front of it.
        assert_eq!(command_line("worker 9f8e7d6c5b4a39281706f5e4d3c2b1a0"), "worker [redacted]");
        assert_eq!(command_line("app --user me@example.com"), "app --user [redacted]");
    }

    #[test]
    fn urls_keep_only_their_origin() {
        let out = command_line("agent --server https://user:pw@example.com:8443/path?key=secret#frag");
        assert_eq!(out, "agent --server https://example.com:8443");
    }

    #[test]
    fn long_command_lines_are_capped() {
        let line = format!("chrome {}", (0..200).map(|i| format!("--flag-{i}")).collect::<Vec<_>>().join(" "));
        let out = command_line(&line);
        assert!(out.chars().count() <= 321);
        assert!(out.contains("more") || out.ends_with('…'));
    }

    #[test]
    fn ordinary_words_and_numbers_stay() {
        assert_eq!(command_line("python -m vllm.entrypoints.openai.api_server --model Qwen/Qwen3-8B"),
            "python -m vllm.entrypoints.openai.api_server --model Qwen3-8B");
    }
}
