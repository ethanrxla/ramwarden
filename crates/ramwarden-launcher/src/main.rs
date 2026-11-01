//! `ramwarden-launcher`: see, install and update wardens.
//!
//! ```text
//! ramwarden-launcher                     status of every warden (checks first)
//! ramwarden-launcher check [--background]
//! ramwarden-launcher update [all|<id>]   install newer versions
//! ramwarden-launcher install <id>        add a warden, e.g. wazuh (Pro)
//!
//! ramwarden-launcher release keygen
//! ramwarden-launcher release manifest --warden ram --name "RAM Warden" --tier free \
//!     --version 0.3.0 --dist dist --base-url https://…/v0.3.0 [--previous old.json] \
//!     [--health URL] [--icon URL] [--summary TEXT] --out dist/manifest.json
//! ramwarden-launcher release sign dist/manifest.json      # key from $RAMWARDEN_SIGNING_KEY
//! ramwarden-launcher release verify dist/manifest.json    # against this build's keys
//! ```
//!
//! `--background` is what the scheduled check runs: silent unless something
//! changed, honours `[updates].mode`, and installs on its own only what needs
//! no password or UAC prompt (the tarball installs on macOS and Linux).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ramwarden_launcher::fetch::Http;
use ramwarden_launcher::manifest::{Format, Tier};
use ramwarden_launcher::release::Spec;
use ramwarden_launcher::signing::Signer;
use ramwarden_launcher::state::{State, device_token};
use ramwarden_launcher::target::Target;
use ramwarden_launcher::updater::{RAM, update_order};
use ramwarden_launcher::{Applied, Error, Manifest, Updater, Verifier};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match words.as_slice() {
        ["--version" | "-V"] => {
            println!("ramwarden-launcher {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["release", rest @ ..] => release(rest),
        [] | ["status"] => status(),
        ["check"] => check(false),
        ["check", "--background"] => check(true),
        ["update"] | ["update", "all"] => update(None),
        ["update", id] | ["install", id] => update(Some(id)),
        _ => {
            eprintln!("usage: ramwarden-launcher [status|check [--background]|update [all|<id>]|install <id>|release …]");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn updater() -> (Updater<Http>, ramwarden_core::config::Updates) {
    let cfg = ramwarden_core::config::load()
        .map(|c| c.updates)
        .unwrap_or_else(|e| {
            eprintln!("config: {e}; using default update settings");
            Default::default()
        });
    let up = Updater {
        fetch: Http::default(),
        verifier: Verifier::shipped(),
        target: Target::current(),
        manifest_url: cfg.manifest_url.clone(),
        dir: State::default_dir(),
        device_token: device_token(),
        own_version: semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver"),
        is_root: is_root(),
    };
    (up, cfg)
}

#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
fn is_root() -> bool {
    false
}

/// Check and persist; the verified manifest is cached for `status` offline.
fn checked(up: &Updater<Http>) -> (State, Result<Manifest, Error>) {
    let mut state = State::load(&up.dir);
    let m = up.check(&mut state);
    if let Err(e) = state.save(&up.dir) {
        eprintln!("could not save launcher state: {e}");
    }
    (state, m)
}

fn status() -> Result<(), Error> {
    let (up, _) = updater();
    let (state, m) = checked(&up);
    let running = m.as_ref().map(|m| up.running(m)).unwrap_or_default();
    println!("ramwarden-launcher {} on {}", env!("CARGO_PKG_VERSION"), up.target.triple());
    if let Err(e) = &m {
        println!("update check: {e}");
    }
    for a in &state.available {
        let run = match running.iter().find(|(id, _)| id == &a.id) {
            Some((_, true)) => "running",
            Some((_, false)) => "stopped",
            None => "",
        };
        let what = match (&a.installed, a.update) {
            (Some(v), true) => format!("{v} -> {} available", a.latest),
            (Some(v), false) => format!("{v} (up to date)"),
            (None, _) if !a.supported => "not built for this machine".to_string(),
            (None, _) if a.locked => format!("{} (needs Pro)", a.latest),
            (None, _) => format!("{} (not installed: ramwarden-launcher install {})", a.latest, a.id),
        };
        let tier = if a.tier == Tier::Pro { " [Pro]" } else { "" };
        println!("  {:<16} {:<44} {run}", format!("{}{tier}", a.name), what);
    }
    if state.available.is_empty() && m.is_err() {
        println!("  {:<16} {} (installed)", "RAM Warden", env!("CARGO_PKG_VERSION"));
    }
    Ok(())
}

fn check(background: bool) -> Result<(), Error> {
    let (up, cfg) = updater();
    if background && cfg.mode == "off" {
        return Ok(());
    }
    let (mut state, m) = checked(&up);
    let m = match m {
        Ok(m) => m,
        // A scheduled check with no network or no key is not worth a log line
        // every six hours; `status` shows the stored error.
        Err(_) if background => return Ok(()),
        Err(e) => return Err(e),
    };
    let ids = update_order(&state.available);
    if ids.is_empty() {
        if !background {
            println!("everything is up to date");
        }
        return Ok(());
    }
    for id in &ids {
        let a = state.available.iter().find(|a| &a.id == id).expect("from available");
        println!("{} {} -> {}", a.name, a.installed.as_deref().unwrap_or("-"), a.latest);
    }
    if background && cfg.mode == "install" {
        // Only what installs without a prompt: nobody is there to answer one.
        if up.target.formats.first() == Some(&Format::TarGz) {
            for id in ids {
                report(up.apply(&m, &id, &mut state));
            }
            state.save(&up.dir)?;
        }
    } else if !background {
        println!("run `ramwarden-launcher update` to install");
    }
    Ok(())
}

fn update(only: Option<&str>) -> Result<(), Error> {
    let (up, _) = updater();
    let (mut state, m) = checked(&up);
    let m = m?;
    let ids = match only {
        Some(id) => vec![id.to_string()],
        None => update_order(&state.available),
    };
    if ids.is_empty() {
        println!("everything is up to date");
    }
    let mut failed = None;
    for id in ids {
        let r = up.apply(&m, &id, &mut state);
        let started_self = matches!(&r, Ok(Applied::Started { id, .. }) if id == RAM);
        if let Err(e) = &r {
            failed = Some(Error::Install(format!("{id}: {e}")));
        }
        report(r);
        state.save(&up.dir)?;
        if started_self {
            // The installer is about to replace this executable.
            break;
        }
    }
    failed.map_or(Ok(()), Err)
}

fn report(r: Result<Applied, Error>) {
    match r {
        Ok(Applied::Installed { id, version }) => println!("{id}: installed {version}"),
        Ok(Applied::Started { id, version }) => {
            println!("{id}: installer for {version} started; it finishes in the background")
        }
        Ok(Applied::UpToDate { id, version }) => println!("{id}: {version} is current"),
        Err(e) => eprintln!("{e}"),
    }
}

// ── release ─────────────────────────────────────────────────────────────────

fn release(words: &[&str]) -> Result<(), Error> {
    match words {
        ["keygen"] => {
            let s = Signer::generate()?;
            println!("public  (add to crates/ramwarden-launcher/update-keys.txt):\n  {}", s.public_b64());
            println!("private (GitHub secret RAMWARDEN_SIGNING_KEY; never commit):\n  {}", s.seed_b64());
            Ok(())
        }
        ["sign", file] => {
            let seed = std::env::var("RAMWARDEN_SIGNING_KEY")
                .map_err(|_| Error::Unsupported("set RAMWARDEN_SIGNING_KEY to the private key".into()))?;
            let bytes = std::fs::read(file)?;
            let sig = Signer::from_seed_b64(&seed)?.sign(&bytes);
            std::fs::write(format!("{file}.sig"), format!("{sig}\n"))?;
            println!("wrote {file}.sig");
            Ok(())
        }
        ["verify", file] => {
            let bytes = std::fs::read(file)?;
            let sig = std::fs::read_to_string(format!("{file}.sig"))?;
            Verifier::shipped().verify(&bytes, &sig)?;
            Manifest::parse(&bytes)?;
            println!("{file}: signature and contents OK");
            Ok(())
        }
        ["manifest", flags @ ..] => manifest(flags),
        _ => Err(Error::Unsupported("release keygen | manifest … | sign <file> | verify <file>".into())),
    }
}

fn manifest(flags: &[&str]) -> Result<(), Error> {
    let get = |name: &str| -> Option<String> {
        flags
            .windows(2)
            .find(|w| w[0] == name)
            .map(|w| w[1].to_string())
    };
    let need = |name: &str| get(name).ok_or_else(|| Error::Unsupported(format!("release manifest needs {name}")));
    let tier = match need("--tier")?.as_str() {
        "free" => Tier::Free,
        "pro" => Tier::Pro,
        t => return Err(Error::Unsupported(format!("tier {t}: free or pro"))),
    };
    let spec = Spec {
        id: need("--warden")?,
        name: need("--name")?,
        tier,
        version: need("--version")?,
        icon: get("--icon"),
        summary: get("--summary"),
        health: get("--health"),
        base_url: need("--base-url")?,
    };
    let previous = match get("--previous") {
        Some(p) if Path::new(&p).exists() => Some(Manifest::parse(&std::fs::read(&p)?)?),
        _ => None,
    };
    let published_at = get("--published-at").unwrap_or_else(|| "unknown".to_string());
    let m = spec.manifest(&PathBuf::from(need("--dist")?), previous, published_at)?;
    let out = need("--out")?;
    std::fs::write(&out, serde_json::to_vec_pretty(&m).map_err(std::io::Error::other)?)?;
    println!("wrote {out}: sequence {}, {} wardens", m.sequence, m.wardens.len());
    Ok(())
}
