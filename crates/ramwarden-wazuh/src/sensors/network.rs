//! A bounded local connection snapshot. No DNS resolution, packet payloads,
//! browsing history or upload. Visibility is limited to the current OS user.
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Network {
    pub checked_at: u64,
    pub connections: Vec<Connection>,
    pub problem: Option<String>,
    pub scope: &'static str,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Connection {
    pub pid: u32,
    pub process: String,
    pub local: String,
    pub remote: String,
}

pub async fn collect() -> Network {
    let mut network = Network {
        checked_at: super::now(),
        scope: "Established TCP connections visible to this user; UDP and other users may be absent. Addresses stay on this computer.",
        ..Default::default()
    };
    match command().await {
        Ok(text) => match parse(&text, cfg!(windows)) {
            Ok(rows) => network.connections = rows,
            Err(e) => network.problem = Some(e),
        },
        Err(e) => network.problem = Some(e),
    }
    network
}

async fn command() -> Result<String, String> {
    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("powershell.exe");
        c.args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; $names=@{}; Get-Process | ForEach-Object {$names[[int]$_.Id]=$_.ProcessName}; $rows=@(Get-NetTCPConnection | Where-Object {$_.State -eq 'Established'} | Select-Object -First 513 | ForEach-Object {@{pid=[int]$_.OwningProcess;process=[string]$names[[int]$_.OwningProcess];local=('{0}:{1}' -f $_.LocalAddress,$_.LocalPort);remote=('{0}:{1}' -f $_.RemoteAddress,$_.RemotePort)}}); ConvertTo-Json -InputObject $rows -Compress"]);
        #[cfg(windows)]
        c.creation_flags(0x08000000);
        c
    } else {
        let mut c = tokio::process::Command::new(if cfg!(target_os = "macos") {
            "/usr/sbin/lsof"
        } else {
            "lsof"
        });
        c.args(["-nP", "-iTCP", "-sTCP:ESTABLISHED", "-Fpcn"]);
        c
    };
    cmd.kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|_| "Connection inventory tool unavailable".to_string())?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or("Connection output unavailable")?
        .take(256 * 1024 + 1);
    let result = tokio::time::timeout(Duration::from_secs(12), async {
        let mut data = Vec::new();
        stdout
            .read_to_end(&mut data)
            .await
            .map_err(|_| "Connection read failed")?;
        if data.len() > 256 * 1024 {
            return Err("Connection inventory exceeded its size limit");
        }
        let status = child
            .wait()
            .await
            .map_err(|_| "Connection inventory failed")?;
        // lsof returns 1 when no connections match; empty is a valid snapshot.
        if !status.success() && !(cfg!(unix) && status.code() == Some(1) && data.is_empty()) {
            return Err("Connection inventory command failed");
        }
        String::from_utf8(data).map_err(|_| "Invalid connection output")
    })
    .await
    .map_err(|_| "Connection inventory timed out".to_string())?;
    result.map_err(str::to_string)
}

fn parse(text: &str, windows: bool) -> Result<Vec<Connection>, String> {
    let mut rows: Vec<Connection> = if windows {
        serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .map_err(|_| "Invalid connection snapshot".to_string())?
    } else {
        let mut rows = Vec::new();
        let mut pid = 0;
        let mut process = String::new();
        for line in text.lines() {
            if let Some(p) = line.strip_prefix('p') {
                pid = p.parse().unwrap_or(0);
                process.clear();
            } else if let Some(p) = line.strip_prefix('c') {
                process = p.to_string();
            } else if let Some((local, remote)) =
                line.strip_prefix('n').and_then(|s| s.split_once("->"))
                && pid > 0
            {
                    rows.push(Connection {
                        pid,
                        process: process.clone(),
                        local: local.into(),
                        remote: remote.into(),
                    });
            }
        }
        rows
    };
    if rows.len() > 512 {
        return Err(
            "More than 512 connections; snapshot omitted rather than presented as complete".into(),
        );
    }
    rows.sort_by(|a, b| (&a.process, a.pid, &a.remote).cmp(&(&b.process, b.pid, &b.remote)));
    rows.dedup();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ipv6_names_and_pid_boundaries() {
        let rows = parse("p12\ncBrowser Helper\nn[::1]:30->[2001:db8::1]:443\np13\ncTerminal\nn127.0.0.1:2->127.0.0.1:3\n", false).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].pid, 12);
        assert_eq!(rows[0].remote, "[2001:db8::1]:443");
    }
    #[test]
    fn windows_empty_and_invalid_are_different() {
        assert!(parse("[]", true).unwrap().is_empty());
        assert!(parse("Access denied", true).is_err());
        assert!(parse("[{\"pid\":1}]", true).is_err());
    }
}
