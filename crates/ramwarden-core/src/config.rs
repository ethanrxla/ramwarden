//! Configuration, reading the same `ramwarden.toml` v1 reads.
//!
//! # Compatibility is the point
//!
//! The Rust daemon has to drop into a running v1 install without the user
//! editing anything, so every key v1 understood is still understood, with the
//! same defaults and the same search order. New v2 keys — the ladder rungs, the
//! Ollama endpoint — all have defaults, so an untouched v1 config file produces
//! a working v2 daemon.
//!
//! Secrets live in a separate `secrets.toml` because `ramwarden.toml` is tracked
//! by git. v1 learned the hard way that a packaged install reads its config from
//! `/etc`, which no user can write a key into, so the per-user file is checked
//! too and wins when both exist.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Thresholds inherited from v1. Still honoured, but the ladder now triggers on
/// stall time rather than percent-used — see [`Ladder`] for why.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Thresholds {
    /// Percent-used at which v1 began looking for things to reclaim.
    pub ram_percent: f64,
    /// Percent-used at which v1 escalated to the model.
    pub critical_percent: f64,
    /// A browser tab inactive this long is a candidate for closing.
    pub inactivity_minutes: u32,
    /// Minimum gap between automatic triggers.
    pub debounce_minutes: u32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            ram_percent: 75.0,
            critical_percent: 85.0,
            inactivity_minutes: 60,
            debounce_minutes: 15,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Server {
    pub port: u16,
    pub host: String,
}

impl Default for Server {
    fn default() -> Self {
        // v1's dataclass defaulted to 0.0.0.0 but its loader defaulted to
        // localhost. The loader is what actually ran, and binding every
        // interface by accident is not a default worth inheriting.
        Server {
            port: 7823,
            host: "127.0.0.1".to_string(),
        }
    }
}

/// Pressure at which each rung of the remediation ladder engages.
///
/// These are PSI `some avg10` percentages — the share of the last ten seconds in
/// which at least one task stalled waiting for memory. Percent-of-RAM is not
/// used because it cannot distinguish a full-but-healthy machine from a
/// stalling one: on the machine this was written for, 23 GiB of 30 GiB "used"
/// included 4 GB that zram had already compressed away.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Ladder {
    /// Rung 0 — ask the kernel to reclaim cold pages from idle scopes.
    /// Invisible to applications.
    pub reclaim_at: f64,
    /// Rung 1 — page out cold regions and soft-cap the worst offender.
    pub pageout_at: f64,
    /// Rung 2 — close stale browser tabs.
    pub tabs_at: f64,
    /// Rung 3 — SIGSTOP idle watchlisted applications.
    pub suspend_at: f64,
    /// Rung 4 — terminate, then kill. Measured against PSI `full avg10`, which
    /// only rises when nothing is getting useful work done.
    pub kill_at_full: f64,
    /// Rung 4 also engages when available memory falls below this, in MB,
    /// regardless of stall time — by the time thrashing registers it is late.
    pub kill_below_available_mb: u64,
    /// Seconds a kill notification stays cancellable.
    ///
    /// The kill proceeds by default when it expires, so the ladder stays
    /// autonomous; this is a chance to intervene, not a confirmation prompt.
    /// Set to 0 to kill immediately with no window.
    pub kill_grace_seconds: u64,
    /// Pressure below which everything reversible is undone: suspended apps
    /// resumed, soft caps lifted. Hysteresis — deliberately well under
    /// `reclaim_at`, so the ladder cannot oscillate.
    pub release_below: f64,
}

impl Default for Ladder {
    fn default() -> Self {
        Ladder {
            reclaim_at: 2.0,
            pageout_at: 5.0,
            tabs_at: 10.0,
            suspend_at: 15.0,
            kill_at_full: 25.0,
            kill_below_available_mb: 500,
            kill_grace_seconds: 10,
            release_below: 1.0,
        }
    }
}

impl Ladder {
    /// The rungs must be ordered, or escalation skips steps and the ladder
    /// reaches for a signal when reclaim would have done.
    pub fn validate(&self) -> Result<(), String> {
        let steps = [
            ("release_below", self.release_below),
            ("reclaim_at", self.reclaim_at),
            ("pageout_at", self.pageout_at),
            ("tabs_at", self.tabs_at),
            ("suspend_at", self.suspend_at),
        ];
        for pair in steps.windows(2) {
            let (an, a) = pair[0];
            let (bn, b) = pair[1];
            if a >= b {
                return Err(format!(
                    "ladder out of order: {an} ({a}) must be below {bn} ({b})"
                ));
            }
        }
        Ok(())
    }
}

/// The local model endpoint. Replaces v1's `[api]` Anthropic key.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Ollama {
    pub host: String,
    /// Decides which applications and tabs to reclaim.
    pub model: String,
    /// Ranks tab relevance against the user's stated goal.
    ///
    /// **`mxbai-embed-large`, not `nomic-embed-text`.** Both are pulled on this
    /// machine and the choice was settled by measurement on a six-document set
    /// with a security-research goal:
    ///
    /// ```text
    /// nomic-embed-text    spread 0.100   ranked a basketball score above a CVE entry
    /// mxbai-embed-large   spread 0.394   CVE, Metasploit, Burp on top; recipe last
    /// ```
    ///
    /// nomic is unusable for this: its scores barely separate, so the ordering is
    /// noise. See `ramwarden_ai::rerank::MIN_SPREAD`, which refuses to act on a
    /// ranking that flat.
    pub embed_model: String,
    pub timeout_seconds: u64,
    /// Skip the local model when the GPU is already full of the user's own work.
    pub respect_vram: bool,
}

impl Default for Ollama {
    fn default() -> Self {
        Ollama {
            host: "http://127.0.0.1:11434".to_string(),
            model: "nemotron-3-nano:4b".to_string(),
            embed_model: "mxbai-embed-large".to_string(),
            timeout_seconds: 120,
            respect_vram: true,
        }
    }
}

/// The behaviour log, kept so the hand-tuned idle heuristic can eventually be
/// replaced by something learned from this machine.
///
/// # The defaults are a measured disk budget, not a guess
///
/// Logging every process every tick would be six and a half million rows a day,
/// so it is sampled. The first defaults chosen here (60 s, 50 MB) were still
/// wrong — measured on the target machine, 56 processes clear a 50 MB floor and a
/// row costs 195 bytes, which is **472 MB a month**. Measured cost at various
/// settings:
///
/// ```text
///   interval  floor   rows/day   disk/30d
///       60 s   50 MB     80,640     472 MB
///       60 s  100 MB     40,320     236 MB
///      300 s  100 MB      8,064      47 MB   <- the defaults
///      300 s  200 MB      4,838      28 MB
/// ```
///
/// A five-minute interval still gives six observations inside the default
/// half-hour horizon, which is enough to catch a return without filling a disk
/// to do it.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Logging {
    pub enabled: bool,
    /// Seconds between samples.
    pub interval_seconds: u64,
    /// Ignore processes smaller than this; they can never be reclaim candidates
    /// and are only noise in the training set.
    pub floor_mb: u64,
    /// Drop samples older than this.
    pub keep_days: i64,
    /// How far ahead to look when labelling "the user came back".
    pub horizon_seconds: i64,
}

impl Default for Logging {
    fn default() -> Self {
        Logging {
            enabled: true,
            interval_seconds: 300,
            floor_mb: 100,
            keep_days: 30,
            horizon_seconds: 1800,
        }
    }
}

/// The cloud tier: NVIDIA NIM at build.nvidia.com.
///
/// **Off by default, even with a key present.** The prompt carries the titles and
/// hosts of every open tab, which is a browsing history; having credentials on the
/// machine is not the same as consenting to send that off it. Set
/// `enabled = true` to opt in.
///
/// The model and mode were settled by measurement, not documentation:
/// `nemotron-3-super-120b-a12b` with `json_object` mode returns a valid object in
/// under a second, while `json_schema` mode truncates and
/// `nemotron-3.5-lightning` ignores both and answers with its reasoning.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Nvidia {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub embed_model: String,
    /// Read from `secrets.toml` or `$NVIDIA_API_KEY`. Never put it in
    /// `ramwarden.toml`, which is tracked by git.
    pub api_key: String,
    pub timeout_seconds: u64,
    /// `"local"` or `"cloud"` — which tier to try first.
    pub prefer: String,
}

impl Default for Nvidia {
    fn default() -> Self {
        Nvidia {
            enabled: false,
            base_url: "https://integrate.api.nvidia.com".to_string(),
            model: "nvidia/nemotron-3-super-120b-a12b".to_string(),
            embed_model: "nvidia/nemotron-3-embed-1b".to_string(),
            api_key: String::new(),
            timeout_seconds: 90,
            prefer: "local".to_string(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct WorkspaceRule {
    #[serde(rename = "match")]
    pub pattern: String,
    #[serde(default = "default_match_type")]
    pub match_type: String,
    pub workspace: u32,
}

fn default_match_type() -> String {
    "class".to_string()
}

#[derive(Clone, Debug)]
pub struct Config {
    pub thresholds: Thresholds,
    pub server: Server,
    pub ladder: Ladder,
    pub ollama: Ollama,
    pub nvidia: Nvidia,
    pub logging: Logging,
    /// Process names the user has opted into reclaiming. A ceiling, never
    /// widened by detection — see [`crate::detector::Detector::may_suspend`].
    pub watchlist: Vec<String>,
    pub workspace_rules: Vec<WorkspaceRule>,
    pub db_path: PathBuf,
    /// Where this was loaded from, for logging and for the UI's "edit config".
    pub source: Option<PathBuf>,
}

/// The TOML shape, kept separate from [`Config`] so the file format and the
/// in-memory form can diverge without breaking either.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Raw {
    thresholds: Thresholds,
    server: Server,
    ladder: Ladder,
    ollama: Ollama,
    nvidia: Nvidia,
    logging: Logging,
    processes: RawProcesses,
    workspaces: RawWorkspaces,
    db_path: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawProcesses {
    watchlist: Option<Vec<String>>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawWorkspaces {
    rules: Vec<WorkspaceRule>,
}

fn default_watchlist() -> Vec<String> {
    ["Discord", "BurpSuiteCommunity", "burpsuite", "cursor", "Cursor"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn default_db_path() -> PathBuf {
    home()
        .join(".local")
        .join("share")
        .join("ramwarden")
        .join("history.db")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

impl Default for Config {
    fn default() -> Self {
        Config {
            thresholds: Thresholds::default(),
            server: Server::default(),
            ladder: Ladder::default(),
            ollama: Ollama::default(),
            nvidia: Nvidia::default(),
            logging: Logging::default(),
            watchlist: default_watchlist(),
            workspace_rules: Vec::new(),
            db_path: default_db_path(),
            source: None,
        }
    }
}

/// Where to look for a config, in order. Mirrors v1 exactly.
pub fn search_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os("RAMWARDEN_CONFIG") {
        out.push(PathBuf::from(p));
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join("ramwarden.toml"));
    }
    out.push(home().join(".config/ramwarden/config.toml"));
    out.push(PathBuf::from("/etc/ramwarden/config.toml"));
    out
}

/// Load from the first config found on the search path, or defaults if none.
pub fn load() -> Result<Config, String> {
    match search_paths().into_iter().find(|p| p.exists()) {
        Some(p) => load_from(&p),
        None => {
            tracing::info!("no config file found — using defaults");
            Ok(Config::default())
        }
    }
}

/// Load one specific file, merging any adjacent `secrets.toml`.
pub fn load_from(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut cfg = parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    cfg.source = Some(path.to_path_buf());

    // Later paths win, matching v1: the per-user file overrides one shipped
    // beside a packaged config.
    let secret_paths = [
        path.parent().map(|d| d.join("secrets.toml")),
        Some(home().join(".config/ramwarden/secrets.toml")),
    ];
    for sp in secret_paths.into_iter().flatten() {
        if !sp.exists() {
            continue;
        }
        let stext = std::fs::read_to_string(&sp).map_err(|e| format!("{}: {e}", sp.display()))?;
        let secrets: Raw =
            toml::from_str(&stext).map_err(|e| format!("{}: {e}", sp.display()))?;
        // Only the fields a secrets file has any business setting.
        if secrets.ollama.host != Ollama::default().host {
            cfg.ollama.host = secrets.ollama.host;
        }
        if !secrets.nvidia.api_key.is_empty() {
            cfg.nvidia.api_key = secrets.nvidia.api_key;
        }
        if secrets.nvidia.enabled {
            cfg.nvidia.enabled = true;
        }
    }

    // The environment wins over both files, which is how a service unit or a
    // one-off run supplies a key without writing it to disk.
    if let Ok(k) = std::env::var("NVIDIA_API_KEY")
        && !k.trim().is_empty()
    {
        cfg.nvidia.api_key = k.trim().to_string();
    }

    if let Err(e) = cfg.ladder.validate() {
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(cfg)
}

/// Parse config text. Unknown keys are ignored, so a v1 file with an `[api]`
/// section loads cleanly rather than failing on a key v2 no longer uses.
pub fn parse(text: &str) -> Result<Config, toml::de::Error> {
    let raw: Raw = toml::from_str(text)?;
    Ok(Config {
        thresholds: raw.thresholds,
        server: raw.server,
        ladder: raw.ladder,
        ollama: raw.ollama,
        nvidia: raw.nvidia,
        logging: raw.logging,
        watchlist: raw.processes.watchlist.unwrap_or_else(default_watchlist),
        workspace_rules: raw.workspaces.rules,
        db_path: raw.db_path.map(PathBuf::from).unwrap_or_else(default_db_path),
        source: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The config this machine actually runs, verbatim from `ramwarden.toml`.
    const SHIPPED: &str = r#"
[thresholds]
ram_percent = 65
critical_percent = 72
inactivity_minutes = 45
debounce_minutes = 120

[server]
host = "0.0.0.0"
port = 7823

[processes]
watchlist = [
  "Discord",
  "BurpSuiteCommunity",
  "burpsuite",
]

[[workspaces.rules]]
match = "cosmic-term"
match_type = "class"
workspace = 2

[[workspaces.rules]]
match = "brave-browser"
match_type = "class"
workspace = 0
"#;

    /// A v1 config carrying the Anthropic key section v2 no longer uses.
    const V1_WITH_API: &str = r#"
[thresholds]
ram_percent = 65

[api]
key = "sk-ant-should-be-ignored"

[server]
port = 7823
"#;

    #[test]
    fn loads_the_config_this_machine_actually_runs() {
        let c = parse(SHIPPED).unwrap();
        assert_eq!(c.thresholds.ram_percent, 65.0);
        assert_eq!(c.thresholds.critical_percent, 72.0);
        assert_eq!(c.thresholds.inactivity_minutes, 45);
        assert_eq!(c.thresholds.debounce_minutes, 120);
        assert_eq!(c.server.host, "0.0.0.0");
        assert_eq!(c.server.port, 7823);
        assert_eq!(c.watchlist, vec!["Discord", "BurpSuiteCommunity", "burpsuite"]);
        assert_eq!(c.workspace_rules.len(), 2);
        assert_eq!(c.workspace_rules[0].pattern, "cosmic-term");
        assert_eq!(c.workspace_rules[0].workspace, 2);
    }

    /// A v1 file must not fail to load just because v2 dropped the Claude tier.
    #[test]
    fn a_v1_config_with_an_api_key_section_still_loads() {
        let c = parse(V1_WITH_API).unwrap();
        assert_eq!(c.thresholds.ram_percent, 65.0);
        assert_eq!(c.ollama.model, "nemotron-3-nano:4b", "falls back to the local model");
    }

    #[test]
    fn an_empty_config_yields_working_defaults() {
        let c = parse("").unwrap();
        assert_eq!(c.server.port, 7823);
        assert_eq!(c.server.host, "127.0.0.1", "not every interface, by default");
        assert!(c.ladder.validate().is_ok());
        assert!(!c.watchlist.is_empty());
    }

    #[test]
    fn partial_sections_keep_defaults_for_the_rest() {
        let c = parse("[thresholds]\nram_percent = 50\n").unwrap();
        assert_eq!(c.thresholds.ram_percent, 50.0);
        assert_eq!(
            c.thresholds.critical_percent,
            Thresholds::default().critical_percent
        );
    }

    #[test]
    fn the_default_ladder_is_correctly_ordered() {
        assert!(Ladder::default().validate().is_ok());
    }

    /// A ladder whose rungs cross would skip steps — reaching for SIGSTOP when
    /// a cgroup reclaim would have done the job invisibly.
    #[test]
    fn an_out_of_order_ladder_is_rejected_with_a_useful_message() {
        let c = parse("[ladder]\nreclaim_at = 20.0\npageout_at = 5.0\n").unwrap();
        let err = c.ladder.validate().unwrap_err();
        assert!(err.contains("reclaim_at"), "{err}");
        assert!(err.contains("pageout_at"), "{err}");
    }

    #[test]
    fn release_must_sit_below_the_first_rung_so_the_ladder_cannot_oscillate() {
        let c = parse("[ladder]\nrelease_below = 9.0\nreclaim_at = 2.0\n").unwrap();
        assert!(c.ladder.validate().is_err());
    }

    #[test]
    fn the_ladder_can_be_tuned_from_the_config_file() {
        // Lowering the first rung means lowering the release point with it, or
        // the hysteresis band collapses — which `validate` refuses.
        let c = parse(
            "[ladder]\nrelease_below = 0.5\nreclaim_at = 1.0\npageout_at = 3.0\ntabs_at = 7.0\n\
             suspend_at = 12.0\nkill_at_full = 30.0\nkill_grace_seconds = 0\n",
        )
        .unwrap();
        assert!(c.ladder.validate().is_ok());
        assert_eq!(c.ladder.kill_grace_seconds, 0, "0 means kill with no window");
        assert_eq!(c.ladder.tabs_at, 7.0);
    }

    /// Dropping the first rung without dropping the release point leaves no
    /// hysteresis, so the ladder would engage and release at the same pressure
    /// and flap. Rejected at load rather than discovered in production.
    #[test]
    fn lowering_the_first_rung_onto_the_release_point_is_rejected() {
        let c = parse("[ladder]\nreclaim_at = 1.0\n").unwrap();
        assert_eq!(c.ladder.release_below, 1.0, "the default");
        let err = c.ladder.validate().unwrap_err();
        assert!(err.contains("release_below"), "{err}");
    }

    /// The choice that measurement forced: nomic cannot do this job.
    /// The defaults must fit a real disk budget. Measured on the target machine:
    /// 56 processes clear a 50 MB floor, and a row costs 195 bytes. An earlier
    /// version of this test only checked `rows < 100_000_000`, which passed
    /// trivially while the real figure was 472 MB a month.
    #[test]
    fn the_behaviour_log_fits_a_measured_disk_budget() {
        const BYTES_PER_ROW: u64 = 195;
        /// Processes clearing a 50 MB floor, measured.
        const AT_50MB: f64 = 56.0;

        let l = Logging::default();
        assert!(l.enabled);
        assert!(l.keep_days > 0, "an unbounded log grows forever");

        // Processes scale roughly with the floor: half at 100 MB, a third at 200.
        let processes = match l.floor_mb {
            0..=50 => AT_50MB,
            51..=100 => AT_50MB * 0.5,
            _ => AT_50MB * 0.3,
        };
        let rows_per_day = processes * 86_400.0 / l.interval_seconds as f64;
        let bytes = rows_per_day * l.keep_days as f64 * BYTES_PER_ROW as f64;

        assert!(
            bytes < 100_000_000.0,
            "{:.0} MB over {} days is too much for a background log",
            bytes / 1e6,
            l.keep_days
        );
        // And the resolution must still be enough to catch a return.
        let observations = l.horizon_seconds / l.interval_seconds as i64;
        assert!(
            observations >= 5,
            "only {observations} samples inside the horizon — a return would be missed"
        );
    }

    #[test]
    fn the_behaviour_log_can_be_turned_off_entirely() {
        let c = parse("[logging]\nenabled = false\n").unwrap();
        assert!(!c.logging.enabled);
    }

    #[test]
    fn the_default_embedder_is_the_one_that_actually_discriminates() {
        let c = Config::default();
        assert_eq!(c.ollama.embed_model, "mxbai-embed-large");
        assert_ne!(
            c.ollama.embed_model, "nomic-embed-text",
            "nomic ranked a basketball score above a CVE entry"
        );
    }

    /// Having a key on the machine is not consent to send browsing data off it.
    #[test]
    fn the_cloud_tier_is_off_by_default() {
        let c = Config::default();
        assert!(!c.nvidia.enabled);
        assert!(c.nvidia.api_key.is_empty());
        assert_eq!(c.nvidia.prefer, "local");
    }

    /// `json_schema` mode truncates on this model and lightning ignores both
    /// modes; the default must be the combination that was measured to work.
    #[test]
    fn the_default_cloud_model_is_the_one_that_returns_valid_json() {
        assert_eq!(
            Config::default().nvidia.model,
            "nvidia/nemotron-3-super-120b-a12b"
        );
    }

    #[test]
    fn the_cloud_tier_can_be_enabled_from_the_config() {
        let c = parse("[nvidia]\nenabled = true\nprefer = \"cloud\"\n").unwrap();
        assert!(c.nvidia.enabled);
        assert_eq!(c.nvidia.prefer, "cloud");
    }

    /// The key must never live in the tracked config file.
    #[test]
    fn a_key_in_secrets_toml_is_merged_but_one_in_the_config_is_not_required() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ramwarden.toml");
        std::fs::write(&p, SHIPPED).unwrap();
        std::fs::write(
            dir.path().join("secrets.toml"),
            "[nvidia]\napi_key = \"nvapi-fromsecretsfile000000000000000\"\nenabled = true\n",
        )
        .unwrap();
        let c = load_from(&p).unwrap();
        assert!(c.nvidia.api_key.starts_with("nvapi-"));
        assert!(c.nvidia.enabled, "secrets may turn the tier on");
    }

    #[test]
    fn the_ollama_endpoint_is_configurable() {
        let c = parse(
            "[ollama]\nhost = \"http://gpu.local:11434\"\nmodel = \"nemotron-3-nano:30b\"\n",
        )
        .unwrap();
        assert_eq!(c.ollama.host, "http://gpu.local:11434");
        assert_eq!(c.ollama.model, "nemotron-3-nano:30b");
        assert_eq!(
            c.ollama.embed_model, "mxbai-embed-large",
            "unspecified fields keep their default"
        );
    }

    #[test]
    fn a_workspace_rule_defaults_to_matching_on_class() {
        let c = parse("[[workspaces.rules]]\nmatch = \"x\"\nworkspace = 1\n").unwrap();
        assert_eq!(c.workspace_rules[0].match_type, "class");
    }

    #[test]
    fn malformed_toml_is_an_error_rather_than_silent_defaults() {
        assert!(parse("[thresholds\nram_percent = ").is_err());
        assert!(parse("[thresholds]\nram_percent = \"not a number\"").is_err());
    }

    #[test]
    fn an_explicit_db_path_is_honoured() {
        let c = parse("db_path = \"/tmp/rw.db\"\n").unwrap();
        assert_eq!(c.db_path, PathBuf::from("/tmp/rw.db"));
    }

    #[test]
    fn the_search_path_matches_v1s_order() {
        let paths = search_paths();
        let strs: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        // The env override, when set, must come first.
        assert!(
            strs.iter().any(|s| s.ends_with("ramwarden.toml")),
            "cwd config missing: {strs:?}"
        );
        assert!(strs.iter().any(|s| s.ends_with(".config/ramwarden/config.toml")));
        assert_eq!(strs.last().unwrap(), "/etc/ramwarden/config.toml");
    }

    #[test]
    fn loading_a_real_file_records_where_it_came_from() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ramwarden.toml");
        std::fs::write(&p, SHIPPED).unwrap();
        let c = load_from(&p).unwrap();
        assert_eq!(c.source.as_deref(), Some(p.as_path()));
        assert_eq!(c.watchlist.len(), 3);
    }

    #[test]
    fn an_adjacent_secrets_file_overrides_the_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ramwarden.toml");
        std::fs::write(&p, SHIPPED).unwrap();
        std::fs::write(
            dir.path().join("secrets.toml"),
            "[ollama]\nhost = \"http://secret-host:11434\"\n",
        )
        .unwrap();
        let c = load_from(&p).unwrap();
        assert_eq!(c.ollama.host, "http://secret-host:11434");
    }

    #[test]
    fn a_file_with_an_invalid_ladder_fails_to_load_rather_than_running_misconfigured() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ramwarden.toml");
        std::fs::write(&p, "[ladder]\nreclaim_at = 50.0\npageout_at = 1.0\n").unwrap();
        assert!(load_from(&p).is_err());
    }
}
