//! The read-only probes the warden answers from.
//!
//! Each probe reads one thing about the computer and returns a small JSON
//! object, capped at [`CAP`] bytes. None of them changes anything. Locks are
//! taken, read and released before any await, so a slow model server never
//! holds up the ladder.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ramwarden_core::detector::Detector;
use serde_json::{Value, json};

use super::redact;
use super::servers;
use crate::hub::AppState;

/// Most bytes one probe may add to the prompt. Several probes plus the
/// overview stay well under the 8 KB that made the router demand compression.
pub const CAP: usize = 2048;
/// How many processes an app probe lists.
const APP_ROWS: usize = 8;
/// Command lines shown per app probe: the largest processes only.
const APP_COMMANDS: usize = 3;
const MB: f64 = 1_048_576.0;

/// One question the warden can ask the computer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Probe {
    /// RAM, pressure, swap and the largest apps. Sent on every turn.
    Overview,
    /// One app by name: its processes and children, memory, CPU, VRAM, ports.
    App(String),
    /// Every GPU: use, VRAM, temperature and who holds the VRAM.
    Gpu,
    /// Loaded local models across Ollama, LM Studio, llama-server and vLLM.
    Models,
    /// What RamWarden has recently done to an app.
    History(String),
    /// This computer's CPU model, cores, RAM and GPUs.
    Hardware,
    /// The user's other devices, from Tailscale.
    Devices,
    /// One other device's hardware, asked over SSH when that is allowed.
    Device(String),
}

impl Probe {
    /// The name shown under the reply ("Looked at: GPU, ollama").
    pub fn label(&self) -> String {
        match self {
            Probe::Overview => "memory".into(),
            Probe::App(n) => n.clone(),
            Probe::Gpu => "GPU".into(),
            Probe::Models => "local models".into(),
            Probe::History(n) => format!("RamWarden's history with {n}"),
            Probe::Hardware => "this computer's hardware".into(),
            Probe::Devices => "your Tailscale devices".into(),
            Probe::Device(n) => format!("{n} (another device)"),
        }
    }

    /// A probe the model asked for: `{"probe":"app","name":"ollama"}`.
    /// Anything else is ignored. A name is data, never an instruction.
    pub fn from_request(v: &Value) -> Option<Probe> {
        let name = || {
            v["name"]
                .as_str()
                .map(str::trim)
                .filter(|n| !n.is_empty() && n.len() <= 64)
                .map(str::to_string)
        };
        match v["probe"].as_str()? {
            "overview" => Some(Probe::Overview),
            "app" => name().map(Probe::App),
            "gpu" => Some(Probe::Gpu),
            "models" => Some(Probe::Models),
            "history" => name().map(Probe::History),
            "hardware" => Some(Probe::Hardware),
            "devices" => Some(Probe::Devices),
            "device" => name().map(Probe::Device),
            _ => None,
        }
    }
}

/// The catalogue as the model sees it.
pub const CATALOG: &str = r#"Read-only probes you may request (at most 3, one round):
{"probe":"app","name":"<app or process name>"}  processes and children, memory MiB, CPU %, VRAM, ports, idle minutes, redacted command lines
{"probe":"gpu"}                                  each GPU's use %, VRAM, temperature and the processes holding VRAM
{"probe":"models"}                               models loaded now in Ollama, LM Studio, llama-server, vLLM and similar
{"probe":"history","name":"<app>"}              what RamWarden recently did to that app
{"probe":"overview"}                             RAM, pressure, swap and the largest apps
{"probe":"hardware"}                             this computer's CPU model, physical and logical cores, RAM and GPUs
{"probe":"devices"}                              the user's other devices on Tailscale: name, OS, online
{"probe":"device","name":"<device name>"}       another device's CPU, cores and RAM, when the user allows asking it"#;

/// Words that are never app names, so "what is my computer doing" does not
/// probe an app called "computer".
const NOT_APPS: &[&str] = &[
    "the", "what", "why", "how", "doing", "using", "computer", "system", "memory", "ram", "gpu", "cpu",
    "app", "apps", "process", "processes", "running", "warden", "mac", "pc", "windows", "linux", "service",
    "server", "helper", "renderer", "python", "node", "java", "host", "agent", "kernel", "init",
];

/// Probes the daemon runs before asking the model, picked from the question
/// alone. Cheap and deterministic, so most questions need one model call.
/// `names` are the running apps and process names, lowercase.
pub fn pick(message: &str, names: &HashSet<String>, devices: &[super::devices::Device]) -> Vec<Probe> {
    let text = message.to_lowercase();
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '.')
        .map(|w| w.trim_matches('.'))
        .filter(|w| !w.is_empty())
        .collect();
    let has = |w: &[&str]| words.iter().any(|x| w.contains(x));
    let mut out = Vec::new();
    if has(&["gpu", "gpus", "vram", "cuda", "graphics", "nvidia", "geforce", "rtx", "radeon", "metal"])
        || text.contains("video card")
        || text.contains("graphics card")
    {
        out.push(Probe::Gpu);
    }
    if has(&["model", "models", "llm", "llms", "ollama", "llama", "llama-server", "lmstudio", "vllm", "inference", "gguf", "qwen", "gemma", "mistral", "deepseek"])
        || text.contains("lm studio")
    {
        out.push(Probe::Models);
    }
    // Another device named in the question ("my legion") comes before this
    // computer's hardware: "how many cores does my legion have" is about it.
    let named: Vec<&super::devices::Device> = words
        .iter()
        .filter(|w| !NOT_APPS.contains(w))
        .filter_map(|w| super::devices::find(devices, w))
        .fold(Vec::new(), |mut acc, d| {
            if !acc.iter().any(|x: &&super::devices::Device| x.name == d.name) {
                acc.push(d);
            }
            acc
        });
    for d in named.iter().take(2) {
        out.push(Probe::Device(d.name.clone()));
    }
    if has(&["devices", "tailscale", "tailnet", "machines", "computers", "laptops", "servers"]) || text.contains("other device") {
        out.push(Probe::Devices);
    }
    if named.is_empty()
        && (has(&["cpu", "cpus", "core", "cores", "processor", "processors", "threads", "hardware", "specs", "spec", "ryzen", "intel", "motherboard"])
            || text.contains("how much ram")
            || text.contains("how much memory do"))
    {
        out.push(Probe::Hardware);
    }
    let history = has(&["closed", "close", "killed", "froze", "frozen", "suspended", "paused", "history", "did"]);
    let mut apps: Vec<String> = Vec::new();
    for w in &words {
        if w.len() < 3 || NOT_APPS.contains(w) || apps.iter().any(|a| a == w) {
            continue;
        }
        if names.contains(*w) {
            apps.push((*w).to_string());
        }
    }
    // Multi-word app names ("google chrome", "visual studio code").
    for n in names.iter().filter(|n| n.contains(' ') && text.contains(n.as_str())) {
        if !apps.iter().any(|a| n.contains(a.as_str())) {
            apps.push(n.clone());
        }
    }
    for a in apps.into_iter().take(2) {
        if history {
            out.push(Probe::History(a.clone()));
        }
        out.push(Probe::App(a));
    }
    out
}

/// Lowercase names of everything running, for [`pick`].
pub fn running_names(det: &Detector) -> HashSet<String> {
    let mut out = HashSet::new();
    for s in det.snapshot().values() {
        out.insert(s.name.trim_end_matches(".exe").to_lowercase());
        if !s.app.is_empty() {
            out.insert(s.app.to_lowercase());
        }
    }
    out
}

/// Run one probe. Always returns an object; a reading that is unavailable
/// says so rather than failing the turn.
pub async fn run(st: &AppState, probe: &Probe) -> Value {
    let v = match probe {
        Probe::Overview => overview(st),
        Probe::App(name) => app(st, name).await,
        Probe::Gpu => gpu(st).await,
        Probe::Models => models(st).await,
        Probe::History(name) => history(st, name),
        Probe::Hardware => hardware(st).await,
        Probe::Devices => devices().await,
        Probe::Device(name) => device(st, name).await,
    };
    cap(v, CAP)
}

/// Shrink `v` until its JSON fits in `max` bytes, dropping the last rows of
/// its longest list first. Marks the result `"truncated": true`.
pub fn cap(mut v: Value, max: usize) -> Value {
    let size = |v: &Value| serde_json::to_string(v).map_or(0, |s| s.len());
    let mut cut = false;
    while size(&v) > max {
        let Some(obj) = v.as_object_mut() else { break };
        let longest = obj
            .iter()
            .filter_map(|(k, x)| x.as_array().filter(|a| !a.is_empty()).map(|a| (k.clone(), a.len())))
            .max_by_key(|(_, n)| *n)
            .map(|(k, _)| k);
        match longest {
            Some(k) => {
                obj.get_mut(&k).and_then(Value::as_array_mut).map(Vec::pop);
                cut = true;
            }
            None => {
                // No list left to shorten: drop the command lines, then give up.
                if obj.remove("commands").is_none() {
                    break;
                }
                cut = true;
            }
        }
    }
    if cut && let Some(obj) = v.as_object_mut() {
        obj.insert("truncated".into(), json!(true));
    }
    v
}

fn mb(bytes: u64) -> f64 {
    (bytes as f64 / MB).round()
}

fn overview(st: &AppState) -> Value {
    let m = st.memory();
    let p = st.pressure();
    let top = st
        .det
        .try_read()
        .ok()
        .map(|det| {
            let mut by_app: HashMap<&str, (u64, usize)> = HashMap::new();
            for s in det.snapshot().values() {
                let key = if s.app.is_empty() { s.name.as_str() } else { s.app.as_str() };
                let e = by_app.entry(key).or_default();
                e.0 += s.pss;
                e.1 += 1;
            }
            let mut rows: Vec<_> = by_app.into_iter().collect();
            rows.sort_by_key(|(_, (b, _))| std::cmp::Reverse(*b));
            rows.into_iter()
                .take(8)
                .map(|(app, (b, n))| json!({"app":app,"memory_mib":mb(b),"processes":n}))
                .collect::<Vec<_>>()
        });
    json!({
        "probe":"overview",
        "ram_used_percent":(m.percent()*10.0).round()/10.0,
        "ram_used_mib":mb(m.used()),"ram_total_mib":mb(m.total),
        "swap_used_mib":mb(m.swap_total.saturating_sub(m.swap_free)),
        "pressure_some_avg10":p.some_avg10,"pressure_full_avg10":p.full_avg10,"pressure_estimated":p.estimated,
        "largest_apps":top,
        "note":"largest_apps sums each app's own memory (PSS or private working set). null means the sample was busy."
    })
}

/// One process, as an app probe reports it.
struct Row {
    pid: i32,
    name: String,
    part: &'static str,
    pss: u64,
    cpu_percent: Option<f64>,
    age_minutes: f64,
    ports: Vec<u16>,
    established: u32,
    focused: bool,
    audio: bool,
    window: bool,
    state: char,
    verdict: &'static str,
    command: Option<String>,
    /// Names from this process up to the root of its tree.
    ancestors: Vec<String>,
}

async fn app(st: &AppState, query: &str) -> Value {
    let q = query.trim().trim_end_matches(".exe").to_lowercase();
    // Read everything under the lock, then release it before any await.
    let gathered = st.det.try_read().ok().map(|det| {
        let snap = det.snapshot();
        let span = det.tick_span().map(|d| d.as_secs_f64()).filter(|s| *s > 0.0);
        let matches = |name: &str, app: &str| {
            let n = name.trim_end_matches(".exe").to_lowercase();
            n.contains(&q) || app.to_lowercase().contains(&q)
        };
        let mut pids: HashSet<i32> = snap.values().filter(|s| matches(&s.name, &s.app)).map(|s| s.pid).collect();
        // Children: llama runners under ollama, renderers under a browser.
        loop {
            let more: Vec<i32> = snap
                .values()
                .filter(|s| !pids.contains(&s.pid) && pids.contains(&s.ppid))
                .map(|s| s.pid)
                .collect();
            if more.is_empty() {
                break;
            }
            pids.extend(more);
        }
        let mut rows: Vec<Row> = pids
            .iter()
            .filter_map(|pid| snap.get(pid))
            .map(|s| Row {
                pid: s.pid,
                name: s.name.clone(),
                part: s.part,
                pss: s.pss,
                cpu_percent: span.map(|t| (s.cpu_seconds_recent / t * 1000.0).round() / 10.0),
                age_minutes: s.age_minutes,
                ports: s.listening_ports.clone(),
                established: s.established,
                focused: s.is_focused,
                audio: s.playing_audio,
                window: s.has_window,
                state: s.state,
                verdict: s.verdict.as_str(),
                command: None,
                ancestors: Vec::new(),
            })
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.pss));
        for r in rows.iter_mut().take(APP_COMMANDS) {
            r.command = det.cmdline(r.pid).map(redact::command_line).filter(|c| !c.is_empty());
            // Who started it: up to six parents, stopping at a loop or the root.
            let mut at = snap.get(&r.pid).map(|s| s.ppid);
            while let Some(parent) = at.and_then(|p| snap.get(&p)) {
                if r.ancestors.len() >= 6 || parent.pid == r.pid {
                    break;
                }
                r.ancestors.push(parent.name.clone());
                at = Some(parent.ppid);
            }
        }
        let apps: HashSet<String> =
            pids.iter().filter_map(|p| snap.get(p)).map(|s| if s.app.is_empty() { s.name.clone() } else { s.app.clone() }).collect();
        let now = Instant::now();
        let idle = apps.iter().filter_map(|a| det.quiet_minutes(a, now)).fold(None, |m: Option<f64>, q| Some(m.map_or(q, |m| m.min(q))));
        let busy_alone = apps.iter().map(|a| det.runaway_minutes(a, now)).fold(0.0, f64::max);
        (rows, idle, busy_alone, span)
    });
    let Some((rows, idle, busy_alone, span)) = gathered else {
        return json!({"probe":"app","name":query,"available":false,"note":"The process sample was busy; ask again."});
    };
    if rows.is_empty() {
        return json!({"probe":"app","name":query,"running":false,
            "note":"No running process matches this name. It may be closed, or named differently."});
    }
    // Inside the largest processes: threads, open handles, files, services.
    // A point query per process, off the runtime (Windows walks the process
    // table for it).
    let inside: HashMap<i32, Value> = {
        let pids: Vec<i32> = rows.iter().take(APP_COMMANDS).map(|r| r.pid).collect();
        let platform = st.platform.clone();
        tokio::task::spawn_blocking(move || {
            pids.into_iter()
                .filter_map(|pid| platform.control.detail(pid).map(|d| (pid, detail_json(&d))))
                .collect()
        })
        .await
        .unwrap_or_default()
    };
    let vram = vram_by_pid().await;
    let total_vram: u64 = rows.iter().filter_map(|r| vram.get(&(r.pid as u32))).sum();
    let found: Vec<servers::Found> = rows
        .iter()
        .filter(|r| servers::is_model_server(&r.name, r.command.as_deref().unwrap_or("")) && !r.ports.is_empty())
        .map(|r| servers::Found { pid: r.pid, name: r.name.clone(), ports: r.ports.clone(), command: r.command.clone().unwrap_or_default() })
        .collect();
    let served = if found.is_empty() { Value::Null } else { json!(servers::inspect(found).await) };
    let total: u64 = rows.iter().map(|r| r.pss).sum();
    let cpu: Option<f64> = rows.iter().map(|r| r.cpu_percent).sum::<Option<f64>>().map(|c| (c * 10.0).round() / 10.0);
    let listed = rows.len();
    json!({
        "probe":"app","name":query,"running":true,"process_count":listed,
        "memory_mib":mb(total),"cpu_percent":cpu,"cpu_window_seconds":span.map(|s| s.round()),
        "vram_mib":if vram.is_empty() {Value::Null} else {json!(mb(total_vram))},
        "idle_minutes":idle.map(|m| m.round()),"busy_unattended_minutes":(busy_alone > 0.0).then(|| busy_alone.round()),
        "model_servers":served,
        "processes":rows.iter().take(APP_ROWS).map(|r| json!({
            "pid":r.pid,"name":r.name,"part":r.part,"memory_mib":mb(r.pss),"cpu_percent":r.cpu_percent,
            "vram_mib":vram.get(&(r.pid as u32)).map(|b| mb(*b)),"age_minutes":r.age_minutes.round(),
            "listening_ports":r.ports,"connections":r.established,"focused":r.focused,"audio":r.audio,
            "window":r.window,"suspended":r.state=='T',"classification":r.verdict,
            "command":r.command,"started_by":r.ancestors,"inside":inside.get(&r.pid),
        })).collect::<Vec<_>>(),
        "note":"cpu_percent is per core (100 = one full core) over the last sample window. idle_minutes counts only user-facing use (focus, audio), and only since RamWarden started. Commands are redacted."
    })
}

/// A process's detail as a model may see it: counts as they are, files cut
/// to their names (paths never leave the machine), at most eight of them.
pub fn detail_json(d: &ramwarden_platform::ProcessDetail) -> Value {
    let mut files: Vec<String> = Vec::new();
    for path in &d.open_files {
        let name = redact::command_line(&format!("x {path}"));
        let name = name.strip_prefix("x ").unwrap_or(&name).to_string();
        if !name.is_empty() && !files.contains(&name) {
            files.push(name);
        }
        if files.len() >= 8 {
            break;
        }
    }
    json!({"threads":d.threads,"open_handles":d.open_handles,"open_file_names":files,"services":d.services})
}

/// VRAM per pid on every GPU. NVML is synchronous, so it runs off the runtime.
async fn vram_by_pid() -> HashMap<u32, u64> {
    tokio::task::spawn_blocking(|| {
        let mut out = HashMap::new();
        for g in ramwarden_ai::vram::gpus() {
            for p in ramwarden_ai::vram::processes(g.index) {
                *out.entry(p.pid).or_insert(0) += p.used;
            }
        }
        out
    })
    .await
    .unwrap_or_default()
}

async fn gpu(st: &AppState) -> Value {
    let hw = tokio::task::spawn_blocking(|| {
        let gpus = ramwarden_ai::vram::gpus();
        let procs: Vec<Vec<_>> = gpus.iter().map(|g| ramwarden_ai::vram::processes(g.index)).collect();
        let apple = if gpus.is_empty() { ramwarden_ai::vram::apple_gpu() } else { None };
        (gpus, procs, apple)
    });
    let Ok(Ok((gpus, procs, apple))) = tokio::time::timeout(Duration::from_secs(3), hw).await else {
        return json!({"probe":"gpu","available":false,"note":"The GPU did not answer in time."});
    };
    if let Some(a) = apple {
        return json!({"probe":"gpu","kind":"apple_unified","model":a.model,"utilisation_percent":a.utilisation,
            "gpu_memory_in_use_mib":a.in_use.map(mb),
            "note":"Apple silicon shares RAM with the GPU. macOS does not report per-app GPU use without administrator rights, so only totals are known."});
    }
    if gpus.is_empty() {
        return json!({"probe":"gpu","available":false,
            "note":"No GPU reading is available: no NVIDIA driver (NVML) was found. Other GPUs are not read yet."});
    }
    let names: HashMap<i32, (String, String)> = st
        .det
        .try_read()
        .ok()
        .map(|det| det.snapshot().values().map(|s| (s.pid, (s.name.clone(), s.app.clone()))).collect())
        .unwrap_or_default();
    let cards = gpus
        .iter()
        .zip(procs)
        .map(|(g, ps)| {
            json!({
                "index":g.index,"name":g.name,"utilisation_percent":g.utilisation,"temperature_c":g.temperature_c,
                "vram_used_mib":mb(g.used),"vram_total_mib":mb(g.total),"vram_used_percent":(g.percent_used()*10.0).round()/10.0,
                "processes":ps.iter().take(10).map(|p| {
                    let (name, app) = names.get(&(p.pid as i32)).cloned().unwrap_or_else(|| ("unknown (not in RamWarden's sample)".into(), String::new()));
                    json!({"pid":p.pid,"name":name,"app":app,"vram_mib":mb(p.used)})
                }).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    json!({"probe":"gpu","gpus":cards,
        "note":"utilisation_percent is the driver's compute-busy figure over its last interval. Process VRAM can sum to less than vram_used: the driver and desktop hold the rest."})
}

async fn models(st: &AppState) -> Value {
    let loaded = tokio::time::timeout(Duration::from_secs(3), st.models.loaded()).await.ok();
    let installed = match &st.ai.ollama {
        Some(o) => tokio::time::timeout(Duration::from_secs(2), o.models()).await.ok().and_then(Result::ok),
        None => None,
    };
    let found: Vec<servers::Found> = st
        .det
        .try_read()
        .ok()
        .map(|det| {
            det.snapshot()
                .values()
                .filter(|s| !s.listening_ports.is_empty())
                .filter_map(|s| {
                    let command = det.cmdline(s.pid).map(redact::command_line).unwrap_or_default();
                    servers::is_model_server(&s.name, &command).then(|| servers::Found {
                        pid: s.pid,
                        name: s.name.clone(),
                        ports: s.listening_ports.clone(),
                        command,
                    })
                })
                .take(4)
                .collect()
        })
        .unwrap_or_default();
    let servers = servers::inspect(found).await;
    json!({
        "probe":"models",
        "loaded":loaded.map(|l| l.into_iter().map(|m| json!({"runtime":m.host,"model":m.name,"size_mib":m.bytes.map(mb)})).collect::<Vec<_>>()),
        "ollama_installed":installed.map(|v| v.into_iter().take(12).collect::<Vec<_>>()),
        "other_servers":servers,
        "note":"loaded is what Ollama and LM Studio hold in memory now; installed models are only on disk. other_servers are llama-server, vLLM and similar, asked on their own ports. null means that runtime could not be asked."
    })
}

async fn hardware(st: &AppState) -> Value {
    let hw = st.platform.hardware.read().ok();
    let gpus = tokio::task::spawn_blocking(|| {
        let mut names: Vec<Value> = ramwarden_ai::vram::gpus().iter().map(|g| json!({"name":g.name,"vram_total_mib":mb(g.total)})).collect();
        if names.is_empty()
            && let Some(a) = ramwarden_ai::vram::apple_gpu()
        {
            names.push(json!({"name":a.model,"shared_memory":true}));
        }
        names
    })
    .await
    .unwrap_or_default();
    let m = st.memory();
    json!({
        "probe":"hardware","os":st.platform.name,
        "cpu_model":hw.as_ref().and_then(|h| h.cpu_model.clone()),
        "physical_cores":hw.as_ref().and_then(|h| h.physical_cores),
        "logical_cores":hw.as_ref().and_then(|h| h.logical_cores),
        "performance_cores":hw.as_ref().and_then(|h| h.performance_cores),
        "efficiency_cores":hw.as_ref().and_then(|h| h.efficiency_cores),
        "ram_total_mib":mb(m.total),"gpus":gpus,
        "note":"physical_cores are real cores; logical_cores are the hardware threads the OS schedules on. null means this OS did not report it."
    })
}

async fn devices() -> Value {
    match super::devices::list().await {
        Ok(list) => json!({"probe":"devices","devices":list,
            "note":"Devices on the user's Tailscale network. this_device marks the computer RamWarden is running on."}),
        Err(why) => json!({"probe":"devices","available":false,"note":why.describe()}),
    }
}

async fn device(st: &AppState, name: &str) -> Value {
    let list = match super::devices::list().await {
        Ok(list) => list,
        Err(why) => return json!({"probe":"device","name":name,"read":false,"note":why.describe()}),
    };
    match super::devices::find(&list, name) {
        Some(d) => super::devices::hardware(d, st.cfg.assistant.devices_ssh).await,
        None => json!({"probe":"device","name":name,"read":false,
            "note":"No device with that name is on the user's Tailscale network, so it was not contacted.",
            "known_devices":list.iter().map(|d| d.name.clone()).collect::<Vec<_>>()}),
    }
}

fn history(st: &AppState, name: &str) -> Value {
    let q = name.to_lowercase();
    let rows = st.history.try_lock().ok().and_then(|h| h.recent_actions(200).ok());
    let Some(rows) = rows else {
        return json!({"probe":"history","name":name,"available":false});
    };
    let hits: Vec<Value> = rows
        .into_iter()
        .filter(|a| a.target.to_lowercase().contains(&q))
        .take(6)
        .map(|a| json!({"at":a.at,"action":a.action,"succeeded":a.succeeded,"freed_mib":mb(a.bytes_freed.max(0) as u64)}))
        .collect();
    json!({"probe":"history","name":name,"recent_actions":hits,
        "note":"Only actions RamWarden recorded. An empty list means it has not acted on this app recently."})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn gpu_and_model_questions_pick_their_probes() {
        let running = names(&["ollama", "llama-server", "chrome", "google chrome"]);
        let pick = |m: &str, n: &HashSet<String>| super::pick(m, n, &[]);
        assert_eq!(pick("what is my GPU doing?", &running), vec![Probe::Gpu]);
        assert_eq!(pick("how many CPU cores do I have?", &running), vec![Probe::Hardware]);
        assert_eq!(
            pick("what is ollama doing on my computer", &running),
            vec![Probe::Models, Probe::App("ollama".into())]
        );
        assert_eq!(pick("which model is running?", &running), vec![Probe::Models]);
        assert_eq!(pick("why is Google Chrome so big", &running), vec![Probe::App("chrome".into())]);
        assert!(pick("how is my computer?", &running).is_empty());
    }

    #[test]
    fn history_questions_add_the_history_probe() {
        let running = names(&["slack"]);
        assert_eq!(
            pick("did you close slack?", &running, &[]),
            vec![Probe::History("slack".into()), Probe::App("slack".into())]
        );
    }

    #[test]
    fn a_named_device_is_asked_instead_of_this_computer() {
        let d = |name: &str, this_device| super::super::devices::Device { name: name.into(), os: "linux".into(), address: None, online: true, this_device };
        let devices = [d("RXLA_PC_1", true), d("Ethan-Legion5", false)];
        assert_eq!(
            pick("how many CPU cores does my legion have?", &HashSet::new(), &devices),
            vec![Probe::Device("Ethan-Legion5".into())]
        );
        assert_eq!(pick("what devices are on my tailscale?", &HashSet::new(), &devices), vec![Probe::Devices]);
    }

    #[test]
    fn open_files_reach_a_model_as_names_only() {
        let d = ramwarden_platform::ProcessDetail {
            threads: Some(23),
            open_handles: Some(410),
            open_files: vec!["/home/ethan/models/qwen3-8b.gguf".into(), "/home/ethan/models/qwen3-8b.gguf".into(), "/home/ethan/.ssh/id_ed25519".into()],
            services: vec!["ollama.service".into()],
        };
        let v = detail_json(&d);
        assert_eq!(v["open_file_names"], json!(["qwen3-8b.gguf", "id_ed25519"]));
        assert!(!v.to_string().contains("/home/ethan"));
        assert_eq!(v["threads"], 23);
    }

    #[test]
    fn requests_from_the_model_are_validated() {
        assert_eq!(Probe::from_request(&json!({"probe":"app","name":"ollama"})), Some(Probe::App("ollama".into())));
        assert_eq!(Probe::from_request(&json!({"probe":"gpu"})), Some(Probe::Gpu));
        assert_eq!(Probe::from_request(&json!({"probe":"app"})), None);
        assert_eq!(Probe::from_request(&json!({"probe":"shell","name":"rm -rf"})), None);
        assert_eq!(Probe::from_request(&json!({"probe":"app","name":"x".repeat(65)})), None);
    }

    #[test]
    fn cap_trims_the_longest_list() {
        let v = json!({"probe":"app","processes":(0..200).map(|i| json!({"pid":i,"name":"renderer"})).collect::<Vec<_>>()});
        let out = cap(v, CAP);
        assert!(serde_json::to_string(&out).unwrap().len() <= CAP);
        assert_eq!(out["truncated"], true);
        assert!(!out["processes"].as_array().unwrap().is_empty());
        let small = cap(json!({"probe":"gpu"}), CAP);
        assert!(small.get("truncated").is_none());
    }
}
