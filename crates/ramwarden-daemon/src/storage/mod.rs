//! Disk space: what is using it, a warning before it runs out, and help
//! getting some back.
//!
//! # The three parts
//!
//! 1. **Watch.** The monitor calls [`Storage::check`] about once a minute:
//!    one free-space reading per volume, through the platform's
//!    `StorageSource`. `ramwarden_core::storage::Watch` decides whether that
//!    is worth an alert, and rations it. The alert is the warden's bubble
//!    with its baa.
//! 2. **Explain.** [`Storage::scan`] measures the known places
//!    ([`catalog`]) and the top folders of the home folder ([`scan`]) inside
//!    a time budget. It runs only when someone asks or an alert fires, never
//!    on a timer: walking a disk is the expensive kind of monitoring.
//! 3. **Free.** [`Storage::propose`] turns regenerable places into a
//!    [`cleanup::Proposal`]. [`Storage::apply`] deletes, and only for a
//!    proposal the user approved. Personal files and anything needing
//!    administrator rights are explained, never touched.
//!
//! # What leaves this computer
//!
//! [`Usage::view`] has two forms. The local apps get paths. Models and agents
//! get labels, sizes and the last part of a folder's name, unless
//! `[storage] share_paths` is on. Spec: `docs/specs/storage.md`.

pub mod catalog;
pub mod cleanup;
pub mod routes;
pub mod scan;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ramwarden_core::config;
use ramwarden_core::storage::{Alert, Level, Reading, Watch, size_text};
use ramwarden_platform::{Outcome, Platform};
use serde_json::{Value, json};

use catalog::{Kind, Places};
use cleanup::{Line, Proposal, State};
use scan::Budget;

/// Entries one scan may visit, whatever the clock says.
const SCAN_ENTRIES: i64 = 20_000_000;
/// Home folders listed in a scan.
const TOP_FOLDERS: usize = 12;
/// Build-output folders listed in a scan, and the smallest worth listing.
const TOP_BUILDS: usize = 8;
const MIN_BUILD_BYTES: u64 = 200 * 1_048_576;
/// Places smaller than this are not offered for clearing: not worth a click.
const MIN_CLEAR_BYTES: u64 = 50 * 1_048_576;
/// A scan this fresh answers the next question without another walk.
pub const FRESH: Duration = Duration::from_secs(10 * 60);
/// Proposals remembered, newest last.
const PROPOSALS: usize = 8;

/// One measured place: a catalogue item or a build-output folder.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub label: String,
    pub kind: Kind,
    pub bytes: u64,
    /// False when the budget ran out inside it: `bytes` is a floor.
    pub complete: bool,
    pub paths: Vec<PathBuf>,
    pub note: String,
    pub steps: Vec<String>,
    pub aged: bool,
    pub tagged: bool,
}

/// One top-level folder of the home folder.
#[derive(Clone, Debug)]
pub struct Folder {
    pub path: PathBuf,
    pub bytes: u64,
    pub complete: bool,
}

/// What a scan found.
#[derive(Clone, Debug, Default)]
pub struct Usage {
    /// Seconds since the Unix epoch.
    pub at: u64,
    pub took_ms: u64,
    /// The budget ran out: sizes marked incomplete are floors.
    pub partial: bool,
    pub home: PathBuf,
    pub folders: Vec<Folder>,
    pub places: Vec<Row>,
    /// Folders left out because reading them makes the OS ask the user
    /// (`Places::private`). Empty unless that is so.
    pub private: Vec<PathBuf>,
}

fn name_of(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

impl Usage {
    /// What was left out, as the user would name it: the home folder's own
    /// folders by name, and the rest as one line.
    pub fn not_measured(&self) -> Vec<String> {
        let mut out: Vec<String> = self.private.iter().filter(|p| p.parent() == Some(self.home.as_path())).map(|p| name_of(p)).collect();
        if self.private.iter().any(|p| p.parent() != Some(self.home.as_path())) {
            out.push("other apps' data".into());
        }
        out
    }

    /// Bytes RamWarden could clear after approval.
    pub fn clearable(&self) -> u64 {
        self.clear_rows().map(|r| r.bytes).sum()
    }

    /// The regenerable places worth offering.
    pub fn clear_rows(&self) -> impl Iterator<Item = &Row> {
        self.places.iter().filter(|r| r.kind == Kind::Clear && r.bytes >= MIN_CLEAR_BYTES)
    }

    /// The scan as JSON. `paths` off is the form models and agents get.
    pub fn view(&self, paths: bool) -> Value {
        let place = |r: &Row| {
            let mut v = json!({
                "id": r.id, "label": r.label, "kind": r.kind, "bytes": r.bytes, "size": size_text(r.bytes),
                "complete": r.complete, "note": r.note, "steps": r.steps,
            });
            if paths {
                v["paths"] = json!(r.paths);
            }
            v
        };
        let folder = |f: &Folder| {
            let mut v = json!({"name": name_of(&f.path), "bytes": f.bytes, "size": size_text(f.bytes), "complete": f.complete});
            if paths {
                v["path"] = json!(f.path);
            }
            v
        };
        json!({
            "scanned_at": self.at,
            "took_ms": self.took_ms,
            "partial": self.partial,
            "home_folders": self.folders.iter().map(folder).collect::<Vec<_>>(),
            "places": self.places.iter().filter(|r| r.bytes > 0 || r.paths.is_empty()).map(place).collect::<Vec<_>>(),
            "clearable_bytes": self.clearable(),
"clearable": size_text(self.clearable()),
            "not_measured": self.not_measured(),
            "note": "kind clear: RamWarden can delete the contents after the user approves a proposal. kind review: the user's own files, never deleted by RamWarden. kind guide: the user follows the steps. complete false: the scan ran out of time or left out a private folder, and the size is a minimum. not_measured: folders macOS asks permission for, left out until the user gives RamWarden Full Disk Access.",
        })
    }
}

/// The volumes as JSON.
pub fn volumes_view(readings: &[Reading]) -> Vec<Value> {
    readings
        .iter()
        .map(|r| {
            json!({
                "name": r.name, "system": r.system, "level": r.level,
                "total_bytes": r.total, "free_bytes": r.free,
                "total": size_text(r.total), "free": size_text(r.free),
                "free_percent": if r.total == 0 { 0.0 } else { (r.free as f64 / r.total as f64 * 1000.0).round() / 10.0 },
            })
        })
        .collect()
}

/// Why a proposal could not be made or applied.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No scan has run yet: there are no sizes to propose from.
    NotScanned,
    /// None of the ids named something clearable.
    NothingToClear,
    UnknownProposal,
    /// Already applied, declined or expired.
    NotPending(State),
}

impl Refusal {
    pub fn text(&self) -> String {
        match self {
            Refusal::NotScanned => "Nothing has been measured yet. Ask what is using the space first.".into(),
            Refusal::NothingToClear => "None of those can be cleared by RamWarden. Only places of kind clear can.".into(),
            Refusal::UnknownProposal => "That clean-up is not one I proposed.".into(),
            Refusal::NotPending(State::Expired) => "That offer has expired. Ask again and I will measure afresh.".into(),
            Refusal::NotPending(State::Declined) => "That clean-up was declined.".into(),
            Refusal::NotPending(_) => "That clean-up has already been done.".into(),
        }
    }
}

/// The storage warden's state. One per daemon, shared by routes and monitor.
/// Each lock guards one small value and is never held across a scan, a
/// delete or an await.
#[derive(Default)]
pub struct Storage {
    places: Places,
    watch: Mutex<Watch>,
    checked: Mutex<Option<Instant>>,
    usage: Mutex<Option<Arc<Usage>>>,
    proposals: Mutex<Vec<Proposal>>,
}

impl Storage {
    pub fn new(places: Places) -> Self {
        Storage { places, ..Default::default() }
    }

    // ── Watch ───────────────────────────────────────────────────────────────

    /// Read free space if `check_seconds` have passed, and return an alert if
    /// one is due. Cheap enough for every monitor tick.
    pub fn check(&self, cfg: &config::Storage, platform: &Platform, now: Instant, unix: u64) -> Option<Alert> {
        {
            let mut checked = self.checked.lock().unwrap();
            if checked.is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(cfg.check_seconds.max(5))) {
                return None;
            }
            *checked = Some(now);
        }
        // Unsupported or unreadable: no volumes, so no alert. Degrade quietly.
        let volumes = platform.storage.volumes().ok()?;
        self.watch.lock().unwrap().observe(cfg, &volumes, unix)
    }

    /// Read free space now, without alerting, for a question or a status call.
    pub fn refresh(&self, cfg: &config::Storage, platform: &Platform) -> Vec<Reading> {
        match platform.storage.volumes() {
            Ok(volumes) => {
                // `enabled: false` reads levels but can never return an alert,
                // so a status call does not swallow one the monitor owes.
                let quiet = config::Storage { enabled: false, ..cfg.clone() };
                let mut w = Watch::default();
                w.observe(&quiet, &volumes, 0);
                w.readings().to_vec()
            }
            Err(_) => Vec::new(),
        }
    }

    /// The worst level as of the monitor's last reading.
    pub fn level(&self) -> Level {
        self.watch.lock().unwrap().level()
    }

    // ── Explain ─────────────────────────────────────────────────────────────

    /// The last scan, if it is no older than `max_age`.
    /// A scan that left out folders which are now readable (Full Disk Access
    /// was granted since) is stale whatever its age.
    pub fn cached(&self, max_age: Duration, unix: u64) -> Option<Arc<Usage>> {
        let private = self.places.clone().guarded().private;
        self.usage.lock().unwrap().clone().filter(|u| unix.saturating_sub(u.at) <= max_age.as_secs() && u.private == private)
    }

    /// Measure the known places and the home folder. Blocking: call it from
    /// `spawn_blocking`. Takes at most about `seconds`.
    pub fn scan(&self, seconds: u64, unix: u64) -> Arc<Usage> {
        let started = Instant::now();
        // Asked afresh each scan: Full Disk Access can be granted while the
        // daemon runs, and the next scan should include what it opens.
        let here = self.places.clone().guarded();
        let budget = Budget::new(Duration::from_secs(seconds.clamp(1, 120)), SCAN_ENTRIES).skipping(&here.private);
        let mut places: Vec<Row> = Vec::new();
        for item in here.items() {
            // A place macOS guards (Downloads, Docker's data) is named in
            // `not_measured`, not listed at 0 bytes.
            if !item.paths.is_empty() && item.paths.iter().all(|t| here.template_private(t)) {
                continue;
            }
            let paths: Vec<PathBuf> = item.paths.iter().flat_map(|t| here.resolve(t)).collect();
            let mut bytes = 0;
            let mut complete = true;
            for p in &paths {
                let m = scan::measure_alone(p, &budget);
                bytes += m.bytes;
                complete &= m.complete;
            }
            places.push(Row {
                id: item.id.into(),
                label: item.label.into(),
                kind: item.kind,
                bytes,
                complete,
                paths,
                note: item.note.into(),
                steps: item.steps.iter().map(|s| s.to_string()).collect(),
                aged: item.aged,
                tagged: false,
            });
        }

        let caches = Mutex::new(Vec::new());
        let has_home = here.home.parent().is_some();
        let children = if has_home { scan::children(&here.home, &budget, &caches) } else { Vec::new() };
        let folders = children
            .into_iter()
            .filter(|c| c.measured.bytes > 0)
            .take(TOP_FOLDERS)
            .map(|c| Folder { path: c.path, bytes: c.measured.bytes, complete: c.measured.complete })
            .collect();

        // Tagged caches outside the catalogue's own folders: build output.
        let covered: Vec<PathBuf> = places.iter().filter(|r| r.kind == Kind::Clear).flat_map(|r| r.paths.clone()).collect();
        let mut builds: Vec<scan::CacheDir> = caches
            .into_inner()
            .unwrap()
            .into_iter()
            .filter(|c| c.bytes >= MIN_BUILD_BYTES && !covered.iter().any(|p| c.path.starts_with(p)))
            .collect();
        builds.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
        for (i, b) in builds.into_iter().take(TOP_BUILDS).enumerate() {
            let project = b.path.parent().map(name_of).unwrap_or_default();
            places.push(Row {
                id: format!("build-{}", i + 1),
                label: format!("Build output: {} in {project}", name_of(&b.path)),
                kind: Kind::Clear,
                bytes: b.bytes,
                complete: true,
                paths: vec![b.path],
                note: "A folder that marks itself as a cache (CACHEDIR.TAG). The next build of that project starts from scratch.".into(),
                steps: Vec::new(),
                aged: false,
                tagged: true,
            });
        }
        places.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.id.cmp(&b.id)));

        let usage = Arc::new(Usage {
            at: unix,
            took_ms: started.elapsed().as_millis() as u64,
            partial: budget.spent(),
            home: here.home.clone(),
            folders,
            places,
            private: here.private.clone(),
        });
        *self.usage.lock().unwrap() = Some(usage.clone());
        usage
    }

    // ── Free ────────────────────────────────────────────────────────────────

    /// Propose clearing these places (ids from the last scan), or every
    /// clearable place when `ids` is empty. Nothing is deleted.
    pub fn propose(&self, ids: &[String], unix: u64) -> Result<Proposal, Refusal> {
        let usage = self.usage.lock().unwrap().clone().ok_or(Refusal::NotScanned)?;
        let lines: Vec<Line> = usage
            .places
            .iter()
            .filter(|r| r.kind == Kind::Clear && r.bytes > 0)
            .filter(|r| if ids.is_empty() { r.bytes >= MIN_CLEAR_BYTES } else { ids.contains(&r.id) })
            .map(|r| Line { id: r.id.clone(), label: r.label.clone(), bytes: r.bytes, paths: r.paths.clone(), aged: r.aged, tagged: r.tagged })
            .collect();
        if lines.is_empty() {
            return Err(Refusal::NothingToClear);
        }
        let proposal = Proposal::new(lines, unix);
        let mut all = self.proposals.lock().unwrap();
        all.push(proposal.clone());
        let extra = all.len().saturating_sub(PROPOSALS);
        all.drain(..extra);
        Ok(proposal)
    }

    /// A proposal and its state now.
    pub fn proposal(&self, id: &str, unix: u64) -> Option<Proposal> {
        let mut all = self.proposals.lock().unwrap();
        let p = all.iter_mut().find(|p| p.id == id)?;
        p.age(unix);
        Some(p.clone())
    }

    /// The user said no.
    pub fn decline(&self, id: &str, unix: u64) -> Result<Proposal, Refusal> {
        self.settle(id, unix, State::Declined)
    }

    fn settle(&self, id: &str, unix: u64, to: State) -> Result<Proposal, Refusal> {
        let mut all = self.proposals.lock().unwrap();
        let p = all.iter_mut().find(|p| p.id == id).ok_or(Refusal::UnknownProposal)?;
        p.age(unix);
        if p.state != State::Pending {
            return Err(Refusal::NotPending(p.state.clone()));
        }
        p.state = to;
        Ok(p.clone())
    }

    /// Delete what an approved proposal lists. Blocking. A proposal applies
    /// once: it leaves `Pending` before the first file goes, so a double
    /// click cannot run it twice.
    pub fn apply(&self, id: &str, cfg: &config::Storage, unix: u64) -> Result<(Proposal, Outcome), Refusal> {
        let claimed = self.settle(id, unix, State::Applied { freed_bytes: 0, skipped: 0, notes: Vec::new() })?;
        let min_age = Duration::from_secs_f64(cfg.temp_min_age_hours.max(0.0) * 3600.0);
        let (outcome, skipped) = cleanup::apply(&self.places, &claimed.lines, min_age);
        tracing::info!(
            "storage: cleared {} from {} place(s), {skipped} item(s) kept; {}",
            size_text(outcome.bytes_freed),
            claimed.lines.len(),
            outcome.notes.join("; ")
        );
        // The sizes changed: the next question measures again.
        *self.usage.lock().unwrap() = None;
        let mut all = self.proposals.lock().unwrap();
        let p = all.iter_mut().find(|p| p.id == id).ok_or(Refusal::UnknownProposal)?;
        p.state = State::Applied { freed_bytes: outcome.bytes_freed, skipped, notes: outcome.notes.clone() };
        Ok((p.clone(), outcome))
    }
}

// ── Wording ─────────────────────────────────────────────────────────────────

/// The alert, in the warden's voice. Says the figure, and what it can do.
///
/// No "Baa" in the words: the baa is the goat clip the bubble's `sound`
/// plays. Written into the text, the voice read it out as a word, in a
/// person's voice, over the clip (reference Mac, 2026-11-19).
pub fn alert_text(alert: &Alert, clearable: u64) -> String {
    let v = &alert.volume;
    let lead = match v.level {
        Level::Critical => format!("{} is nearly full: {} left of {}.", v.name, size_text(v.free), size_text(v.total)),
        _ => format!("{} is getting full: {} left of {}.", v.name, size_text(v.free), size_text(v.total)),
    };
    if clearable >= MIN_CLEAR_BYTES {
        format!("{lead} I can clear {} of caches and temp files, nothing of yours. Shall I?", size_text(clearable))
    } else {
        format!("{lead} I found nothing I can safely clear myself, but I can show you what is using it.")
    }
}

/// The scan as plain sentences: the biggest things first, then what to do.
pub fn summary(readings: &[Reading], usage: &Usage) -> String {
    let mut out: Vec<String> = Vec::new();
    for r in readings {
        out.push(format!("{} has {} free of {}.", r.name, size_text(r.free), size_text(r.total)));
    }
    let top: Vec<String> =
        usage.folders.iter().filter(|f| f.bytes >= 1_048_576).take(5).map(|f| format!("{} {}{}", name_of(&f.path), if f.complete { "" } else { "at least " }, size_text(f.bytes))).collect();
    if !top.is_empty() {
        out.push(format!("Biggest folders in your home folder: {}.", top.join(", ")));
    }
    let clear: Vec<String> = usage.clear_rows().take(5).map(|r| format!("{} {}", r.label, size_text(r.bytes))).collect();
    if !clear.is_empty() {
        out.push(format!("I can clear {} after you approve: {}.", size_text(usage.clearable()), clear.join(", ")));
    }
    let guided: Vec<String> = usage
        .places
        .iter()
        .filter(|r| r.kind != Kind::Clear && r.bytes >= MIN_CLEAR_BYTES)
        .take(4)
        .map(|r| format!("{} {}", r.label, size_text(r.bytes)))
        .collect();
    if !guided.is_empty() {
        out.push(format!("Yours to decide, I can walk you through each: {}.", guided.join(", ")));
    }
    let hidden = usage.not_measured();
    if !hidden.is_empty() {
        out.push(format!(
            "I left out {} because macOS asks before any app reads them. To include them, give RamWarden Full Disk Access once in System Settings, Privacy & Security.",
            list_text(&hidden)
        ));
    }
    if usage.partial {
        out.push("I ran out of time measuring, so some sizes are minimums. Ask again for a fuller count.".into());
    }
    out.join(" ")
}

/// "a", "a and b", "a, b and c".
fn list_text(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What clearing did, in the warden's voice.
pub fn result_text(outcome: &Outcome, skipped: u64) -> String {
    if outcome.did_nothing() {
        return "I could not remove anything. Nothing was changed.".into();
    }
    let kept = if skipped > 0 { format!(" {skipped} item(s) were in use, so I left them.") } else { String::new() };
    format!("Done. I cleared {}.{kept}", size_text(outcome.bytes_freed))
}

#[cfg(test)]
mod tests;
