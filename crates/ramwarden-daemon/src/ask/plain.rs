//! Probe readings as plain sentences, with no model.
//!
//! RamWarden Lite has no AI, and a paid plan can still find every model
//! unavailable. Either way the probes have already measured the answer, so
//! the warden states the readings instead of saying it cannot help. Nothing
//! here infers a cause: these are measurements, worded.

use serde_json::Value;

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn gb(mib: f64) -> String {
    if mib >= 1024.0 { format!("{:.1} GB", mib / 1024.0) } else { format!("{mib:.0} MB") }
}

/// One sentence (or a few) per probe result.
pub fn describe(v: &Value) -> Option<String> {
    match v["probe"].as_str()? {
        "overview" => overview(v),
        "app" => app(v),
        "gpu" => gpu(v),
        "models" => models(v),
        "history" => history(v),
        "hardware" => hardware(v),
        "devices" => devices(v),
        "device" => device(v),
        _ => None,
    }
}

fn overview(v: &Value) -> Option<String> {
    let pct = num(&v["ram_used_percent"])?;
    let mut s = format!(
        "RAM: {pct:.0}% in use ({} of {}).",
        gb(num(&v["ram_used_mib"]).unwrap_or(0.0)),
        gb(num(&v["ram_total_mib"]).unwrap_or(0.0))
    );
    if let Some(apps) = v["largest_apps"].as_array().filter(|a| !a.is_empty()) {
        let list = apps
            .iter()
            .take(4)
            .filter_map(|a| Some(format!("{} {}", a["app"].as_str()?, gb(num(&a["memory_mib"])?))))
            .collect::<Vec<_>>()
            .join(", ");
        s += &format!(" Largest: {list}.");
    }
    Some(s)
}

fn app(v: &Value) -> Option<String> {
    let name = v["name"].as_str()?;
    if v["running"] == false {
        return Some(format!("{name}: not running."));
    }
    if v["available"] == false {
        return Some(format!("{name}: the process sample was busy, so ask again in a moment."));
    }
    let mut parts = vec![format!(
        "{} process{}",
        v["process_count"].as_u64().unwrap_or(0),
        if v["process_count"] == 1 { "" } else { "es" }
    )];
    if let Some(m) = num(&v["memory_mib"]) {
        parts.push(format!("{} RAM", gb(m)));
    }
    if let Some(m) = num(&v["vram_mib"]).filter(|m| *m > 0.0) {
        parts.push(format!("{} VRAM", gb(m)));
    }
    if let Some(c) = num(&v["cpu_percent"]) {
        parts.push(format!("{c:.0}% CPU"));
    }
    let models: Vec<String> = v["model_servers"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["models"].as_array().cloned().unwrap_or_default())
        .filter_map(|m| m.as_str().map(str::to_string))
        .collect();
    if !models.is_empty() {
        parts.push(format!("serving {}", models.join(", ")));
    }
    if let Some(i) = num(&v["idle_minutes"]) {
        parts.push(if i < 1.0 { "in use now".into() } else { format!("not used for {i:.0} min") });
    }
    Some(format!("{name}: {}.", parts.join(", ")))
}

fn gpu(v: &Value) -> Option<String> {
    if v["available"] == false {
        return v["note"].as_str().map(str::to_string);
    }
    if v["kind"] == "apple_unified" {
        let mut s = format!("{}:", v["model"].as_str().unwrap_or("GPU"));
        if let Some(u) = num(&v["utilisation_percent"]) {
            s += &format!(" {u:.0}% busy");
        }
        if let Some(m) = num(&v["gpu_memory_in_use_mib"]) {
            s += &format!(", {} of shared memory in use", gb(m));
        }
        return Some(s + ". macOS does not say which apps are using it.");
    }
    let cards = v["gpus"].as_array()?;
    let lines = cards
        .iter()
        .map(|g| {
            let mut s = format!(
                "{}: {:.0}% busy, {} of {} VRAM",
                g["name"].as_str().unwrap_or("GPU"),
                num(&g["utilisation_percent"]).unwrap_or(0.0),
                gb(num(&g["vram_used_mib"]).unwrap_or(0.0)),
                gb(num(&g["vram_total_mib"]).unwrap_or(0.0))
            );
            if let Some(t) = num(&g["temperature_c"]) {
                s += &format!(", {t:.0}°C");
            }
            let holders = g["processes"]
                .as_array()
                .into_iter()
                .flatten()
                .take(3)
                .filter_map(|p| Some(format!("{} {}", p["name"].as_str()?, gb(num(&p["vram_mib"])?))))
                .collect::<Vec<_>>();
            if !holders.is_empty() {
                s += &format!(". Using VRAM: {}", holders.join(", "));
            }
            s + "."
        })
        .collect::<Vec<_>>();
    Some(lines.join(" "))
}

fn models(v: &Value) -> Option<String> {
    let mut found: Vec<String> = Vec::new();
    for m in v["loaded"].as_array().into_iter().flatten() {
        let mut s = format!("{} ({})", m["model"].as_str().unwrap_or("?"), m["runtime"].as_str().unwrap_or("?"));
        if let Some(b) = num(&m["size_mib"]) {
            s += &format!(", {}", gb(b));
        }
        found.push(s);
    }
    for srv in v["other_servers"].as_array().into_iter().flatten() {
        let server = srv["server"].as_str().unwrap_or("server");
        let ids: Vec<&str> = srv["models"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        if ids.is_empty() {
            found.push(format!("{server} is running, model unknown"));
        } else {
            let busy = srv["busy_slots"].as_u64().map_or(String::new(), |n| if n > 0 { ", generating now".into() } else { ", idle".into() });
            found.push(format!("{} ({server}{busy})", ids.join(", ")));
        }
    }
    let asked = !v["loaded"].is_null() || v["other_servers"].as_array().is_some_and(|a| !a.is_empty());
    Some(if found.is_empty() && !asked {
        "Local model runtimes could not be asked right now.".into()
    } else if found.is_empty() {
        "No local model is loaded right now.".into()
    } else {
        format!("Loaded models: {}.", found.join("; "))
    })
}

fn hardware(v: &Value) -> Option<String> {
    let mut s = match v["cpu_model"].as_str() {
        Some(m) => format!("CPU: {m}"),
        None => "CPU".to_string(),
    };
    match (v["physical_cores"].as_u64(), v["logical_cores"].as_u64()) {
        (Some(p), Some(l)) if p != l => s += &format!(", {p} cores ({l} threads)"),
        (Some(p), _) => s += &format!(", {p} cores"),
        (None, Some(l)) => s += &format!(", {l} logical processors"),
        (None, None) => s += ", core count not reported by this system",
    }
    if let (Some(p), Some(e)) = (v["performance_cores"].as_u64(), v["efficiency_cores"].as_u64()) {
        s += &format!(": {p} performance and {e} efficiency");
    }
    s += ".";
    if let Some(m) = num(&v["ram_total_mib"]).filter(|m| *m > 0.0) {
        s += &format!(" RAM: {}.", gb(m));
    }
    let gpus: Vec<&str> = v["gpus"].as_array().into_iter().flatten().filter_map(|g| g["name"].as_str()).collect();
    if !gpus.is_empty() {
        s += &format!(" GPU: {}.", gpus.join(", "));
    }
    Some(s)
}

fn devices(v: &Value) -> Option<String> {
    if v["available"] == false {
        return v["note"].as_str().map(str::to_string);
    }
    let list = v["devices"]
        .as_array()?
        .iter()
        .filter_map(|d| {
            Some(format!(
                "{} ({}, {})",
                d["name"].as_str()?,
                d["os"].as_str().unwrap_or("unknown"),
                if d["this_device"] == true { "this computer" } else if d["online"] == true { "online" } else { "offline" }
            ))
        })
        .collect::<Vec<_>>();
    Some(format!("Your Tailscale devices: {}.", list.join(", ")))
}

fn device(v: &Value) -> Option<String> {
    let name = v["name"].as_str()?;
    if v["read"] == true {
        let lines = v["output"].as_str().unwrap_or("").lines().collect::<Vec<_>>().join("; ");
        return Some(format!("{name} reports: {lines}."));
    }
    Some(format!("{name}: {}", v["note"].as_str().unwrap_or("it could not be asked.")))
}

fn history(v: &Value) -> Option<String> {
    let name = v["name"].as_str()?;
    let rows = v["recent_actions"].as_array()?;
    if rows.is_empty() {
        return Some(format!("RamWarden has not acted on {name} recently."));
    }
    let list = rows
        .iter()
        .take(3)
        .filter_map(|a| {
            Some(format!(
                "{} at {} ({})",
                a["action"].as_str()?,
                a["at"].as_str()?,
                if a["succeeded"].as_bool()? { "worked" } else { "did not work" }
            ))
        })
        .collect::<Vec<_>>()
        .join("; ");
    Some(format!("RamWarden and {name}: {list}."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn app_reading_reads_like_a_sentence() {
        let v = json!({"probe":"app","name":"ollama","running":true,"process_count":2,"memory_mib":5222.0,
            "vram_mib":4400.0,"cpu_percent":2.0,"idle_minutes":14.0,
            "model_servers":[{"models":["qwen3:8b"]}]});
        assert_eq!(
            describe(&v).unwrap(),
            "ollama: 2 processes, 5.1 GB RAM, 4.3 GB VRAM, 2% CPU, serving qwen3:8b, not used for 14 min."
        );
        assert_eq!(describe(&json!({"probe":"app","name":"slack","running":false})).unwrap(), "slack: not running.");
    }

    #[test]
    fn hardware_and_devices_read_plainly() {
        let h = json!({"probe":"hardware","cpu_model":"AMD Ryzen 7 9700X 8-Core Processor","physical_cores":8,"logical_cores":16,
            "ram_total_mib":31846.0,"gpus":[{"name":"NVIDIA GeForce RTX 5070"}]});
        assert_eq!(
            describe(&h).unwrap(),
            "CPU: AMD Ryzen 7 9700X 8-Core Processor, 8 cores (16 threads). RAM: 31.1 GB. GPU: NVIDIA GeForce RTX 5070."
        );
        let d = json!({"probe":"devices","devices":[{"name":"RXLA_PC_1","os":"windows","online":true,"this_device":true},
            {"name":"Ethan-Legion5","os":"linux","online":false,"this_device":false}]});
        assert_eq!(describe(&d).unwrap(), "Your Tailscale devices: RXLA_PC_1 (windows, this computer), Ethan-Legion5 (linux, offline).");
        let one = json!({"probe":"device","name":"Ethan-Legion5","read":true,"output":"16\nmodel name : Intel Core i7"});
        assert_eq!(describe(&one).unwrap(), "Ethan-Legion5 reports: 16; model name : Intel Core i7.");
    }

    #[test]
    fn gpu_and_models_read_plainly() {
        let g = json!({"probe":"gpu","gpus":[{"name":"NVIDIA GeForce RTX 5070","utilisation_percent":37,"temperature_c":61,
            "vram_used_mib":9216.0,"vram_total_mib":16303.0,"processes":[{"name":"llama-server","vram_mib":6144.0}]}]});
        assert_eq!(
            describe(&g).unwrap(),
            "NVIDIA GeForce RTX 5070: 37% busy, 9.0 GB of 15.9 GB VRAM, 61°C. Using VRAM: llama-server 6.0 GB."
        );
        let m = json!({"probe":"models","loaded":[],"other_servers":[{"server":"llama-server","models":["qwen3-8b.gguf"],"busy_slots":0}]});
        assert_eq!(describe(&m).unwrap(), "Loaded models: qwen3-8b.gguf (llama-server, idle).");
        assert_eq!(describe(&json!({"probe":"models","loaded":[],"other_servers":[]})).unwrap(), "No local model is loaded right now.");
        assert_eq!(describe(&json!({"probe":"models","loaded":null,"other_servers":[]})).unwrap(), "Local model runtimes could not be asked right now.");
    }
}
