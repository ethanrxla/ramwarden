//! Installing the Wazuh agent with one admin prompt.
//!
//! The package is Wazuh's own, unmodified, downloaded from the URL the hosted
//! API names and refused unless its SHA-256 matches the one the API sent. It
//! is installed with the OS's own elevation, so the user sees exactly one
//! familiar prompt:
//!
//! | OS | Elevation | Installer |
//! |---|---|---|
//! | Windows | UAC (`Start-Process -Verb RunAs`) | `msiexec /i … /qn WAZUH_MANAGER=…` |
//! | macOS | `osascript … with administrator privileges` | `/tmp/wazuh_envs`, `installer -pkg` |
//! | Linux | `pkexec` (polkit) | `dpkg -i` / `rpm -i` with `WAZUH_MANAGER` set |
//!
//! Then, in the same elevated step, the agent's key from the hosted API is
//! written to its `client.keys` (what `manage_agents -i` does, without the
//! interactive confirmation) and the agent is started. The agent never uses
//! the manager's shared enrolment port.
//!
//! Every value from the API is checked against a strict pattern in
//! [`validate`] before it is put on an elevated command line, so a
//! compromised or buggy API answer cannot inject a command. The file path is
//! ours, and is quoted for each shell.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::cloud::Enrollment;
use crate::{Error, Result};

/// The most a package download may be. Agent packages are 10-30 MB.
const MAX_PACKAGE: usize = 200 * 1024 * 1024;

fn is_host(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && !s.starts_with(['-', '.'])
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Decode and check an agent key. Returns the `client.keys` line.
pub fn key_line(e: &Enrollment) -> Result<String> {
    use base64::Engine;
    let bad = || Error::Parse("cloud", "unsafe agent key in the enrolment".into());
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(e.agent_key.expose().trim())
        .map_err(|_| bad())?;
    let line = String::from_utf8(bytes).map_err(|_| bad())?;
    let parts: Vec<&str> = line.trim().split(' ').collect();
    let [id, name, ip, key] = parts[..] else {
        return Err(bad());
    };
    let ok = id.len() >= 3
        && id.len() <= 10
        && id.chars().all(|c| c.is_ascii_digit())
        && name == e.agent_name
        && (ip == "any"
            || ip.parse::<std::net::IpAddr>().is_ok()
            || ip.contains('/')
                && ip
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() || ".:/".contains(c)))
        && key.len() == 64
        && key.chars().all(|c| c.is_ascii_hexdigit());
    if ok {
        Ok(format!("{id} {name} {ip} {key}"))
    } else {
        Err(bad())
    }
}

/// Refuse an enrolment whose values are not safe to pass to an elevated
/// installer, or whose package is not Wazuh's own over https.
pub fn validate(e: &Enrollment) -> Result<()> {
    let bad = |what: &str| {
        Err(Error::Parse(
            "cloud",
            format!("unsafe {what} in the enrolment"),
        ))
    };
    if !is_host(&e.manager) {
        return bad("manager");
    }
    if !is_name(&e.agent_name) {
        return bad("agent name");
    }
    if !is_name(&e.group) {
        return bad("group");
    }
    key_line(e)?;
    if !matches!(e.package.kind.as_str(), "msi" | "pkg" | "deb" | "rpm") {
        return bad("package kind");
    }
    if e.package.sha256.len() != 64 || !e.package.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return bad("package hash");
    }
    match reqwest::Url::parse(&e.package.url) {
        Ok(u) if u.scheme() == "https" && u.host_str() == Some("packages.wazuh.com") => Ok(()),
        _ => bad("package URL (only https://packages.wazuh.com is accepted)"),
    }
}

/// Download the package and keep it only if its hash matches.
pub async fn download(e: &Enrollment, dir: &Path) -> Result<PathBuf> {
    validate(e)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|err| Error::Install(err.to_string()))?;
    let mut resp = client
        .get(&e.package.url)
        .send()
        .await
        .map_err(|err| Error::Unreachable("package host", err.without_url().to_string()))?;
    if !resp.status().is_success() {
        return Err(Error::Status("package host", resp.status().as_u16()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|err| Error::Unreachable("package host", err.without_url().to_string()))?
    {
        if chunk.len() > MAX_PACKAGE.saturating_sub(bytes.len()) {
            return Err(Error::Parse(
                "package host",
                "package is implausibly large".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    verify(&bytes, &e.package.sha256)?;
    let path = dir.join(format!("ramwarden-wazuh-agent.{}", e.package.kind));
    std::fs::write(&path, &bytes)
        .map_err(|err| Error::Install(format!("{}: {err}", path.display())))?;
    Ok(path)
}

/// Compare a SHA-256, case-insensitively.
pub fn verify(bytes: &[u8], sha256: &str) -> Result<()> {
    let got: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got.eq_ignore_ascii_case(sha256) {
        Ok(())
    } else {
        Err(Error::Install(
            "the downloaded agent package does not match its checksum; not installing it".into(),
        ))
    }
}

/// The `WAZUH_*` settings Wazuh's installers read.
fn settings(e: &Enrollment) -> [(&'static str, &str); 2] {
    [
        ("WAZUH_MANAGER", e.manager.as_str()),
        ("WAZUH_AGENT_NAME", e.agent_name.as_str()),
    ]
}

/// POSIX single-quoting.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// PowerShell single-quoting.
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The program and arguments that install `pkg` elevated on `os`. Pure, so
/// the exact command lines are tested on every platform.
pub fn command(os: &str, e: &Enrollment, pkg: &Path) -> Result<(String, Vec<String>)> {
    validate(e)?;
    let path = pkg.to_string_lossy();
    let line = key_line(e)?;
    match (os, e.package.kind.as_str()) {
        ("windows", "msi") => {
            let mut props: Vec<String> = vec![
                ps_quote("/i"),
                ps_quote(&format!("\"{path}\"")),
                ps_quote("/qn"),
            ];
            props.extend(
                settings(e)
                    .iter()
                    .map(|(k, v)| ps_quote(&format!("{k}={v}"))),
            );
            // Runs elevated. -EncodedCommand carries it without any quoting
            // surviving two command lines.
            let inner = format!(
                "$ErrorActionPreference = 'Stop'; $p = Start-Process -FilePath msiexec.exe -ArgumentList {} -Wait -PassThru; \
                 if ($p.ExitCode -ne 0) {{ exit $p.ExitCode }}; \
                 $dir = Join-Path ${{env:ProgramFiles(x86)}} 'ossec-agent'; \
                 Set-Content -Path (Join-Path $dir 'client.keys') -Value {} -Encoding ascii; \
                 Restart-Service -Name WazuhSvc",
                props.join(","),
                ps_quote(&line)
            );
            let encoded = utf16le_base64(&inner);
            let outer = format!(
                "$ErrorActionPreference = 'Stop'; $p = Start-Process -FilePath powershell.exe -Verb RunAs -WindowStyle Hidden -Wait -PassThru \
                 -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand','{encoded}'; exit $p.ExitCode"
            );
            Ok((
                "powershell.exe".into(),
                vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    outer,
                ],
            ))
        }
        ("macos", "pkg") => {
            // Values passed `validate`, so they hold no quotes or `$`.
            let envs: Vec<String> = settings(e)
                .iter()
                .map(|(k, v)| format!("\"{k}='{v}'\""))
                .collect();
            let script = format!(
                "set -e; umask 077; (set -C; printf '%s\\n' {} > /tmp/wazuh_envs); trap 'rm -f /tmp/wazuh_envs' EXIT; installer -pkg {} -target / && \
                 printf '%s\\n' '{line}' > /Library/Ossec/etc/client.keys && chmod 600 /Library/Ossec/etc/client.keys && /Library/Ossec/bin/wazuh-control restart",
                envs.join(" "),
                sh_quote(&path)
            );
            // AppleScript string: escape backslashes, then double quotes.
            let apple = script.replace('\\', "\\\\").replace('"', "\\\"");
            Ok((
                "osascript".into(),
                vec![
                    "-e".into(),
                    format!(
                        "do shell script \"{apple}\" with administrator privileges \
                         with prompt \"RamWarden needs your permission to install the Wazuh security agent.\""
                    ),
                ],
            ))
        }
        ("linux", kind @ ("deb" | "rpm")) => {
            let envs: Vec<String> = settings(e)
                .iter()
                .map(|(k, v)| format!("{k}={}", sh_quote(v)))
                .collect();
            let install = if kind == "deb" {
                "dpkg -i \"$1\""
            } else {
                "rpm -i \"$1\""
            };
            let script = format!(
                "{} {install} && printf '%s\\n' '{line}' > /var/ossec/etc/client.keys && \
                 chmod 600 /var/ossec/etc/client.keys && systemctl daemon-reload && systemctl enable wazuh-agent && systemctl restart wazuh-agent",
                envs.join(" ")
            );
            Ok((
                "pkexec".into(),
                vec![
                    "sh".into(),
                    "-c".into(),
                    script,
                    "sh".into(),
                    path.into_owned(),
                ],
            ))
        }
        (os, kind) => Err(Error::Install(format!("no {kind} installer on {os}"))),
    }
}

fn utf16le_base64(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Download, verify and install: the one call behind the "Turn on" button.
/// The user sees one OS admin prompt; declining it is an error, not a retry.
pub async fn install(e: &Enrollment) -> Result<()> {
    validate(e)?;
    let dir = tempfile::tempdir().map_err(|err| Error::Install(err.to_string()))?;
    let pkg = download(e, dir.path()).await?;
    let (program, args) = command(std::env::consts::OS, e, &pkg)?;
    let status = wait_for_installer(
        tokio::process::Command::new(&program).args(&args),
        std::time::Duration::from_secs(600),
    ).await;
    let _ = std::fs::remove_file(&pkg);
    match status? {
        s if s.success() => Ok(()),
        s => Err(Error::Install(format!(
            "the agent was not installed (the admin prompt was declined or the installer failed: {s})"
        ))),
    }
}

async fn wait_for_installer(
    command: &mut tokio::process::Command,
    timeout: std::time::Duration,
) -> Result<std::process::ExitStatus> {
    let mut child = command.kill_on_drop(true).spawn()
        .map_err(|err| Error::Install(format!("could not start installer: {err}")))?;
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => result.map_err(|err| Error::Install(format!("installer: {err}"))),
        Err(_) => {
            let _ = child.start_kill();
            Err(Error::Install("Installer timed out. An elevated installer may still be running; check the OS installer before trying again.".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::Package;
    use ramwarden_platform::Secret;

    /// base64 of "042 acct1a2b-desk any " + 64 hex.
    const KEY: &str = "MDQyIGFjY3QxYTJiLWRlc2sgYW55IDAxMjM0NTY3ODlhYmNkZWYwMTIzNDU2Nzg5YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";

    #[tokio::test]
    async fn installer_wait_is_bounded() {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "install::tests::sleeping_installer_child", "--ignored"]);
        let result = wait_for_installer(&mut command, std::time::Duration::from_millis(100)).await;
        assert!(result.unwrap_err().to_string().contains("Installer timed out"));
    }

    #[test]
    #[ignore = "child process for timeout regression test"]
    fn sleeping_installer_child() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    fn enrollment() -> Enrollment {
        Enrollment {
            manager: "agents.ramwarden.com".into(),
            agent_name: "acct1a2b-desk".into(),
            group: "acct-1a2b".into(),
            agent_key: Secret::new(KEY),
            package: Package {
                url: "https://packages.wazuh.com/4.x/windows/wazuh-agent-4.14.0-1.msi".into(),
                sha256: "a".repeat(64),
                kind: "msi".into(),
            },
        }
    }

    #[test]
    fn mac_environment_file_cannot_follow_an_existing_symlink() {
        let mut e = enrollment();
        e.package.kind = "pkg".into();
        let (_, args) = command("macos", &e, Path::new("/tmp/private/agent.pkg")).unwrap();
        let script = args.join(" ");
        assert!(script.contains("set -C"));
        assert!(script.contains("umask 077"));
        assert!(script.contains("chmod 600 /Library/Ossec/etc/client.keys"));
        e.manager = "bad;command".into();
        assert!(command("macos", &e, Path::new("/tmp/private/agent.pkg")).is_err());
    }

    #[test]
    fn a_well_formed_enrollment_is_accepted() {
        validate(&enrollment()).unwrap();
    }

    #[test]
    fn values_that_could_inject_a_command_are_refused() {
        type Mutation = Box<dyn Fn(&mut Enrollment)>;
        let cases: Vec<Mutation> = vec![
            Box::new(|e| e.manager = "x; rm -rf /".into()),
            Box::new(|e| e.manager = "-oProxyCommand".into()),
            Box::new(|e| e.agent_name = "a'b".into()),
            Box::new(|e| e.agent_name = "$(id)".into()),
            Box::new(|e| e.group = "g\"; calc".into()),
            Box::new(|e| e.agent_key = Secret::new("not base64!")),
            // A valid key for another agent name.
            Box::new(|e| e.agent_name = "other".into()),
            // "042 acct1a2b-desk any ';rm -rf /;'"
            Box::new(|e| {
                e.agent_key = Secret::new("MDQyIGFjY3QxYTJiLWRlc2sgYW55ICc7cm0gLXJmIC87Jw==")
            }),
            Box::new(|e| e.package.kind = "exe".into()),
            Box::new(|e| e.package.sha256 = "zz".into()),
            Box::new(|e| e.package.url = "http://packages.wazuh.com/x.msi".into()),
            Box::new(|e| e.package.url = "https://evil.example/x.msi".into()),
        ];
        for (i, f) in cases.iter().enumerate() {
            let mut e = enrollment();
            f(&mut e);
            assert!(validate(&e).is_err(), "case {i} must be refused");
        }
    }

    #[test]
    fn the_agent_key_decodes_to_its_client_keys_line() {
        let line = key_line(&enrollment()).unwrap();
        assert!(line.starts_with("042 acct1a2b-desk any 0123"), "{line}");
    }

    #[test]
    fn the_checksum_must_match() {
        let sha = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"; // "hello"
        verify(b"hello", sha).unwrap();
        verify(b"hello", &sha.to_uppercase()).unwrap();
        assert!(verify(b"hellp", sha).is_err());
    }

    #[test]
    fn windows_runs_msiexec_elevated_with_the_settings() {
        let (prog, args) = command(
            "windows",
            &enrollment(),
            Path::new(r"C:\Users\O'Neil\AppData\Local\Temp\a.msi"),
        )
        .unwrap();
        assert_eq!(prog, "powershell.exe");
        let outer = args.last().unwrap();
        assert!(outer.starts_with("$ErrorActionPreference = 'Stop';"));
        assert!(outer.contains("-Verb RunAs"), "{outer}");
        let encoded = outer
            .split("'-EncodedCommand','")
            .nth(1)
            .unwrap()
            .split('\'')
            .next()
            .unwrap();
        // Decode to check the inner script.
        let inner = decode_utf16le_base64(encoded);
        assert!(inner.starts_with("$ErrorActionPreference = 'Stop';"));
        assert!(
            inner.contains("'WAZUH_MANAGER=agents.ramwarden.com'"),
            "{inner}"
        );
        assert!(
            !inner.contains("REGISTRATION"),
            "no shared enrolment password"
        );
        assert!(
            inner.contains(r#"'"C:\Users\O''Neil\AppData\Local\Temp\a.msi"'"#),
            "{inner}"
        );
        assert!(
            inner.contains("-Value '042 acct1a2b-desk any 0123"),
            "{inner}"
        );
        assert!(inner.contains("Restart-Service -Name WazuhSvc"));
    }

    #[test]
    fn macos_installs_the_pkg_with_one_admin_prompt() {
        let mut e = enrollment();
        e.package.kind = "pkg".into();
        let (prog, args) = command("macos", &e, Path::new("/var/folders/x/T/a.pkg")).unwrap();
        assert_eq!(prog, "osascript");
        let s = &args[1];
        assert!(s.contains("with administrator privileges"));
        assert!(
            s.contains("installer -pkg '/var/folders/x/T/a.pkg' -target /"),
            "{s}"
        );
        assert!(s.contains("WAZUH_MANAGER='agents.ramwarden.com'"), "{s}");
        assert!(s.contains("> /Library/Ossec/etc/client.keys"), "{s}");
        assert!(s.contains("wazuh-control restart"));
    }

    #[test]
    fn linux_uses_pkexec_and_passes_the_path_as_an_argument() {
        for (kind, tool) in [("deb", "dpkg -i \"$1\""), ("rpm", "rpm -i \"$1\"")] {
            let mut e = enrollment();
            e.package.kind = kind.into();
            let (prog, args) = command("linux", &e, Path::new("/tmp/a b.pkg")).unwrap();
            assert_eq!(prog, "pkexec");
            assert_eq!(&args[..2], ["sh", "-c"]);
            assert!(
                args[2].starts_with("WAZUH_MANAGER='agents.ramwarden.com' "),
                "{}",
                args[2]
            );
            assert!(args[2].contains(tool));
            assert!(
                args[2].contains("'042 acct1a2b-desk any 0123"),
                "{}",
                args[2]
            );
            assert!(args[2].ends_with("systemctl restart wazuh-agent"));
            assert_eq!(args[4], "/tmp/a b.pkg", "the path is data, not script");
        }
    }

    #[test]
    fn a_package_for_another_os_is_refused() {
        assert!(command("linux", &enrollment(), Path::new("/tmp/a.msi")).is_err());
    }

    fn decode_utf16le_base64(s: &str) -> String {
        const T: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut bits = 0u32;
        let mut n = 0;
        let mut bytes = Vec::new();
        for c in s.chars().filter(|&c| c != '=') {
            bits = ((bits << 6) | T.find(c).unwrap() as u32) & 0xFFFF;
            n += 6;
            if n >= 8 {
                n -= 8;
                bytes.push((bits >> n) as u8);
            }
        }
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).unwrap()
    }
}
