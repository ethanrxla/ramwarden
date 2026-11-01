//! The personal install line (product spec 2.4 and 5).
//!
//! Once a trial starts, the site shows a one-liner such as
//! `curl -fsSL https://api.ramwarden.app/v1/install/<token>/sh | sh`. The
//! token is a device code that is approved when it is issued. The script
//! saves it as `<config dir>/ramwarden/link-token`, and on first run the app
//! claims it through the normal `POST /v1/device/token`, naming the machine
//! and its OS. So the link is single use, lasts 30 minutes, and puts the new
//! device on the account without a second sign-in. Where nothing reads the
//! token (or it expired), the app falls back to the device sign-in page.
//!
//! The scripts fetch release assets from GitHub (`docs/specs/updates.md`)
//! and check them against the release's `SHA256SUMS` before running
//! anything. Fetching a script does not use up the token. Only a claim does,
//! so a crawler that follows the link takes nothing.

#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use ramwarden_cloud::model::Platform;
use serde_json::json;

use crate::accounts::{Reply, cloud, session, storage};
use crate::routes::{AppState, HOSTED_WARDEN, error};
use crate::wallet::iso8601;

/// How long a personal install line works.
pub const INSTALL_LINK_TTL: u64 = 30 * 60;
/// Where release assets are fetched from when not configured.
pub const DEFAULT_RELEASES_URL: &str = "https://github.com/ethanrxla/ramwarden/releases/latest/download";
/// The name a device gets until the app names it on its first claim.
pub const UNNAMED_DEVICE: &str = "New device";

pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/v1/install/link", post(link))
        .route("/v1/install/{token}/{script}", get(script))
}

/// Exactly the shape the store issues, so nothing else can reach a script.
pub fn valid_token(t: &str) -> bool {
    t.strip_prefix("rwdc_").is_some_and(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

async fn link(State(state): State<Arc<AppState>>, method: Method, headers: HeaderMap) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let a = session(c, &method, &headers, now).await?;
    if !a.active(HOSTED_WARDEN, now) {
        return Err(error(StatusCode::PAYMENT_REQUIRED, "Start a trial or renew to get your install line."));
    }
    if c.store.devices(&a.id).await.map_err(storage)?.len() >= a.device_limit as usize {
        return Err(error(StatusCode::CONFLICT, "This account already has 5 devices. Remove one on your account page first."));
    }
    let (token, dc) = c
        .store
        .start_device_code(UNNAMED_DEVICE, Platform::Linux, now, INSTALL_LINK_TTL)
        .await
        .map_err(storage)?;
    if !c.store.approve_device_code(&dc.user_code, &a.id, now).await.map_err(storage)? {
        return Err(storage("a new install link could not be approved"));
    }
    let base = format!("{}/v1/install/{token}", c.public_url);
    let rel = &c.releases_url;
    Ok(Json(json!({
        "expires_at": iso8601(now + INSTALL_LINK_TTL),
        "commands": {
            "macos": format!("curl -fsSL {base}/sh | sh"),
            "linux": format!("curl -fsSL {base}/sh | sh"),
            "windows": format!("irm {base}/ps1 | iex"),
        },
        // Plain release downloads: a downloaded installer can't carry the
        // token yet, so it signs in through the device page on first run.
        "installers": {
            "macos": format!("{rel}/RamWarden.pkg"),
            "windows": format!("{rel}/ramwarden-windows-x86_64.exe"),
            "linux": format!("{rel}/ramwarden-linux-x86_64.deb"),
        },
    }))
    .into_response())
}

async fn script(State(state): State<Arc<AppState>>, Path((token, kind)): Path<(String, String)>) -> Reply {
    let c = cloud(&state)?;
    let gone = || error(StatusCode::NOT_FOUND, "No such install link.");
    if !valid_token(&token) {
        return Err(gone());
    }
    let body = match kind.as_str() {
        "sh" => sh_script(&token, &c.releases_url),
        "ps1" => ps1_script(&token, &c.releases_url),
        _ => return Err(gone()),
    };
    let mut r = body.into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r)
}

/// macOS and Linux. Interpolates only a checked token and the operator's URL.
pub fn sh_script(token: &str, releases: &str) -> String {
    format!(
        r#"#!/bin/sh
# RamWarden personal installer. This link works once, for 30 minutes, and
# signs this computer in to your account on first run.
set -eu
TOKEN='{token}'
REL='{releases}'
case "$(uname -s)" in
  Darwin) CONF="$HOME/.config/ramwarden"; ASSET=ramwarden-macos-aarch64.tar.gz ;;
  Linux) CONF="${{XDG_CONFIG_HOME:-$HOME/.config}}/ramwarden"; ASSET=ramwarden-linux-x86_64.tar.gz ;;
  *) echo "RamWarden supports macOS and Linux here; on Windows use the PowerShell line." >&2; exit 1 ;;
esac
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
curl -fsSL "$REL/$ASSET" -o "$TMP/$ASSET"
curl -fsSL "$REL/SHA256SUMS" -o "$TMP/SHA256SUMS"
WANT="$(grep " $ASSET\$" "$TMP/SHA256SUMS" | cut -d' ' -f1)"
if command -v sha256sum >/dev/null 2>&1; then GOT="$(sha256sum "$TMP/$ASSET" | cut -d' ' -f1)"; else GOT="$(shasum -a 256 "$TMP/$ASSET" | cut -d' ' -f1)"; fi
if [ -z "$WANT" ] || [ "$WANT" != "$GOT" ]; then echo "Checksum mismatch for $ASSET; nothing was installed." >&2; exit 1; fi
mkdir -p "$CONF"
(umask 077 && printf '%s\n' "$TOKEN" > "$CONF/link-token")
mkdir "$TMP/x"
tar -xzf "$TMP/$ASSET" -C "$TMP/x"
INSTALL="$(find "$TMP/x" -name install.sh -type f | head -n 1)"
(cd "$(dirname "$INSTALL")" && sh ./install.sh)
echo "RamWarden is installed. It signs in to your account when it starts."
"#
    )
}

/// Windows PowerShell.
pub fn ps1_script(token: &str, releases: &str) -> String {
    format!(
        r#"# RamWarden personal installer. This link works once, for 30 minutes, and
# signs this computer in to your account on first run.
$ErrorActionPreference = 'Stop'
$Token = '{token}'
$Rel = '{releases}'
$Asset = 'ramwarden-windows-x86_64.exe'
$Tmp = Join-Path ([IO.Path]::GetTempPath()) ("ramwarden-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $Tmp | Out-Null
$Exe = Join-Path $Tmp $Asset
Invoke-WebRequest -UseBasicParsing "$Rel/$Asset" -OutFile $Exe
$Sums = (Invoke-WebRequest -UseBasicParsing "$Rel/SHA256SUMS").Content -split "`n"
$Want = ($Sums | Where-Object {{ $_ -match " $([regex]::Escape($Asset))$" }} | ForEach-Object {{ ($_ -split ' ')[0] }}) | Select-Object -First 1
$Got = (Get-FileHash -Algorithm SHA256 $Exe).Hash.ToLower()
if (-not $Want -or $Want.Trim().ToLower() -ne $Got) {{ throw "Checksum mismatch for $Asset; nothing was installed." }}
$Conf = Join-Path $env:APPDATA 'ramwarden'
New-Item -ItemType Directory -Force -Path $Conf | Out-Null
Set-Content -Path (Join-Path $Conf 'link-token') -Value $Token -NoNewline
Start-Process -FilePath $Exe -Wait
Remove-Item -Recurse -Force $Tmp
Write-Host 'RamWarden is installed. It signs in to your account when it starts.'
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_issued_tokens_reach_a_script() {
        let t = format!("rwdc_{}", "a1".repeat(32));
        assert!(valid_token(&t));
        for bad in ["rwdc_", "rwdt_aa", &format!("rwdc_{}'", "a".repeat(63)), &format!("rwdc_{}", "A".repeat(64))] {
            assert!(!valid_token(bad), "{bad}");
        }
    }

    #[test]
    fn scripts_check_the_checksum_before_running_anything() {
        let t = format!("rwdc_{}", "0".repeat(64));
        let sh = sh_script(&t, DEFAULT_RELEASES_URL);
        let check = sh.find("Checksum mismatch").unwrap();
        assert!(check < sh.find("link-token\")").unwrap() && check < sh.find("sh ./install.sh").unwrap());
        assert!(sh.contains(&format!("TOKEN='{t}'")));
        let ps = ps1_script(&t, DEFAULT_RELEASES_URL);
        assert!(ps.find("Checksum mismatch").unwrap() < ps.find("Start-Process").unwrap());
    }
}
