//! CPU model and core counts from `/proc/cpuinfo`.
//!
//! Physical cores are the distinct `(physical id, core id)` pairs: x86 lists
//! one block per hardware thread, so counting blocks gives threads, not cores.
//! ARM kernels print no core ids, and there every block is a core.

use std::collections::HashSet;

use ramwarden_platform::Hardware;

use crate::{Error, Result, Root};

/// Parse `/proc/cpuinfo`. Pure, so tests feed it captured text.
pub fn parse(text: &str) -> Hardware {
    let mut model = None;
    let mut logical = 0u32;
    let mut cores: HashSet<(String, String)> = HashSet::new();
    let mut physical_id = String::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "processor" => {
                logical += 1;
                physical_id.clear();
            }
            "model name" | "Model" | "Hardware" if model.is_none() && !value.is_empty() => {
                model = Some(value.to_string());
            }
            "physical id" => physical_id = value.to_string(),
            "core id" => {
                cores.insert((physical_id.clone(), value.to_string()));
            }
            _ => {}
        }
    }
    let logical = (logical > 0).then_some(logical);
    Hardware {
        cpu_model: model,
        physical_cores: if cores.is_empty() { logical } else { Some(cores.len() as u32) },
        logical_cores: logical,
        performance_cores: None,
        efficiency_cores: None,
    }
}

/// Read the machine's hardware under `root`.
pub fn read(root: &Root) -> Result<Hardware> {
    let path = root.join("proc/cpuinfo");
    let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
    let mut hw = parse(&text);
    if hw.cpu_model.is_none() {
        hw.cpu_model = ramwarden_platform::hardware::cpu_brand();
    }
    Ok(hw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(n: u32, core: u32) -> String {
        format!(
            "processor\t: {n}\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen 7 9700X 8-Core Processor\nphysical id\t: 0\ncore id\t\t: {core}\n\n"
        )
    }

    #[test]
    fn smt_threads_are_not_counted_as_cores() {
        // 8 cores, 16 threads: each core id appears twice.
        let text: String = (0..16).map(|n| block(n, n % 8)).collect();
        let hw = parse(&text);
        assert_eq!(hw.cpu_model.as_deref(), Some("AMD Ryzen 7 9700X 8-Core Processor"));
        assert_eq!(hw.physical_cores, Some(8));
        assert_eq!(hw.logical_cores, Some(16));
    }

    #[test]
    fn arm_blocks_without_core_ids_are_cores() {
        let text = "processor\t: 0\nBogoMIPS\t: 48.00\n\nprocessor\t: 1\nBogoMIPS\t: 48.00\n\nHardware\t: BCM2835\n";
        let hw = parse(text);
        assert_eq!(hw.physical_cores, Some(2));
        assert_eq!(hw.cpu_model.as_deref(), Some("BCM2835"));
    }

    #[test]
    fn reads_through_a_synthetic_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("proc")).unwrap();
        std::fs::write(dir.path().join("proc/cpuinfo"), block(0, 0) + &block(1, 1)).unwrap();
        let hw = read(&Root::at(dir.path())).unwrap();
        assert_eq!(hw.physical_cores, Some(2));
    }
}
