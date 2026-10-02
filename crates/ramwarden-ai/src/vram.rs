//! GPU memory, via NVML.
//!
//! # Two reasons RamWarden cares about the card
//!
//! **It is memory, and nothing was watching it.** A browser compositing with
//! hardware acceleration, an Electron app, and a local model all hold VRAM. On a
//! 12 GB card that is a resource worth accounting for, and v1 was entirely blind
//! to it.
//!
//! **It decides whether to ask the model at all.** The local Nemotron needs a few
//! gigabytes of VRAM. If the user's own work already fills the card, loading a
//! model would evict their work — so the provider checks here first and falls
//! back to the heuristic rather than queueing behind them. A RAM manager that
//! degrades the machine to think about managing it has failed at the premise.
//!
//! NVML is absent on any machine without the NVIDIA driver, so every call here
//! degrades to "unknown" rather than failing.

use std::sync::OnceLock;

use nvml_wrapper::Nvml;

/// One GPU's memory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gpu {
    pub index: u32,
    pub name: String,
    pub total: u64,
    pub used: u64,
    /// Percent utilisation of the compute units, if the driver reports it.
    pub utilisation: u32,
}

impl Gpu {
    pub fn free(&self) -> u64 {
        self.total.saturating_sub(self.used)
    }

    pub fn percent_used(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.used as f64 / self.total as f64 * 100.0
    }

    /// Whether a model of `bytes` would fit without evicting anything.
    ///
    /// The margin is not politeness: a model that *just* fits will spill into
    /// system RAM as its KV cache grows, and spilling is the one outcome a memory
    /// manager must not cause.
    pub fn has_room_for(&self, bytes: u64, margin: u64) -> bool {
        self.free() >= bytes.saturating_add(margin)
    }
}

/// One process's VRAM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuProcess {
    pub pid: u32,
    pub used: u64,
}

/// NVML is expensive to initialise and safe to keep, so it is loaded once.
fn nvml() -> Option<&'static Nvml> {
    static NVML: OnceLock<Option<Nvml>> = OnceLock::new();
    NVML.get_or_init(|| match Nvml::init() {
        Ok(n) => Some(n),
        Err(e) => {
            tracing::debug!("NVML unavailable ({e}) — GPU accounting disabled");
            None
        }
    })
    .as_ref()
}

/// Whether this machine has a usable NVIDIA driver.
pub fn available() -> bool {
    nvml().is_some()
}

/// Every GPU's memory. Empty when NVML is unavailable.
pub fn gpus() -> Vec<Gpu> {
    let Some(nvml) = nvml() else {
        return Vec::new();
    };
    let count = nvml.device_count().unwrap_or(0);
    (0..count)
        .filter_map(|i| {
            let d = nvml.device_by_index(i).ok()?;
            let mem = d.memory_info().ok()?;
            Some(Gpu {
                index: i,
                name: d.name().unwrap_or_else(|_| "unknown".into()),
                total: mem.total,
                used: mem.used,
                utilisation: d.utilization_rates().map(|u| u.gpu).unwrap_or(0),
            })
        })
        .collect()
}

/// The first GPU, which is the one a local model will land on.
pub fn primary() -> Option<Gpu> {
    gpus().into_iter().next()
}

/// Per-process VRAM on one GPU.
///
/// Both graphics and compute contexts are consulted, since a browser holds the
/// former and a model holds the latter, and both are memory.
///
/// They must then be **deduplicated by pid**. A process using the GPU for both
/// rendering and compute appears in both lists, and NVML reports its whole
/// footprint in each — so collecting them naively lists it twice and doubles its
/// memory. Observed on this machine: Brave and `cosmic-files` each appeared
/// twice. The larger of the two readings is the right one to keep.
pub fn processes(index: u32) -> Vec<GpuProcess> {
    use std::collections::HashMap;

    let Some(nvml) = nvml() else {
        return Vec::new();
    };
    let Ok(d) = nvml.device_by_index(index) else {
        return Vec::new();
    };

    let mut by_pid: HashMap<u32, u64> = HashMap::new();
    let lists = [
        d.running_graphics_processes().unwrap_or_default(),
        d.running_compute_processes().unwrap_or_default(),
    ];
    for list in lists {
        for p in list {
            // The driver reports "unavailable" for processes it cannot
            // introspect, which is not the same as zero.
            if let nvml_wrapper::enums::device::UsedGpuMemory::Used(bytes) = p.used_gpu_memory {
                let slot = by_pid.entry(p.pid).or_insert(0);
                *slot = (*slot).max(bytes);
            }
        }
    }

    let mut out: Vec<GpuProcess> = by_pid
        .into_iter()
        .map(|(pid, used)| GpuProcess { pid, used })
        .collect();
    out.sort_by_key(|p| std::cmp::Reverse(p.used));
    out
}

/// Total VRAM attributable to processes, deduplicated.
pub fn attributed(index: u32) -> u64 {
    processes(index).iter().map(|p| p.used).sum()
}

/// VRAM headroom to leave free beyond a model's own weights.
///
/// Covers the KV cache, which grows with context length and is the part that
/// pushes a model into system RAM if it was sized to only just fit.
pub const MODEL_MARGIN: u64 = 1_500_000_000;

/// Roughly what `nemotron-3-nano:4b` occupies, from the pulled model's size.
pub const NEMOTRON_4B_BYTES: u64 = 2_800_000_000;

// A KV cache is not small, and a model sized to only just fit spills into system
// RAM as its context grows — the one outcome a memory manager must not cause.
const _: () = assert!(MODEL_MARGIN >= 1_000_000_000);

/// Whether it is reasonable to load the local model right now.
///
/// `None` when there is no GPU to judge — the caller then tries anyway, because
/// Ollama may be running on CPU or on another machine.
pub fn room_for_local_model() -> Option<bool> {
    primary().map(|g| g.has_room_for(NEMOTRON_4B_BYTES, MODEL_MARGIN))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(total: u64, used: u64) -> Gpu {
        Gpu {
            index: 0,
            name: "test".into(),
            total,
            used,
            utilisation: 0,
        }
    }

    #[test]
    fn free_and_percent_are_derived_from_total_and_used() {
        let g = gpu(12_000_000_000, 3_000_000_000);
        assert_eq!(g.free(), 9_000_000_000);
        assert!((g.percent_used() - 25.0).abs() < 0.01);
    }

    #[test]
    fn an_absent_gpu_does_not_divide_by_zero() {
        assert_eq!(Gpu::default().percent_used(), 0.0);
        assert_eq!(Gpu::default().free(), 0);
    }

    #[test]
    fn used_above_total_saturates_rather_than_underflowing() {
        let g = gpu(1_000, 5_000);
        assert_eq!(g.free(), 0);
    }

    /// The scenario this whole module exists for: the card is busy with the
    /// user's work, so loading a model would evict it.
    #[test]
    fn a_busy_card_has_no_room_for_the_model() {
        // 12 GB card with 10 GB in use by the user's work.
        let busy = gpu(12_000_000_000, 10_000_000_000);
        assert!(!busy.has_room_for(NEMOTRON_4B_BYTES, MODEL_MARGIN));

        // The same card as measured on this machine when mostly idle.
        let idle = gpu(12_000_000_000, 1_700_000_000);
        assert!(idle.has_room_for(NEMOTRON_4B_BYTES, MODEL_MARGIN));
    }

    /// A model sized to only just fit spills its KV cache into system RAM, which
    /// is the one outcome a memory manager must not cause.
    #[test]
    fn a_model_that_only_just_fits_is_refused() {
        // Exactly the weights free, and nothing more.
        let tight = gpu(12_000_000_000, 12_000_000_000 - NEMOTRON_4B_BYTES);
        assert!(
            !tight.has_room_for(NEMOTRON_4B_BYTES, MODEL_MARGIN),
            "the margin must keep the KV cache out of system RAM"
        );
        assert!(tight.has_room_for(NEMOTRON_4B_BYTES, 0), "without a margin it fits");
    }

    /// Whatever this machine has, probing must be safe and self-consistent.
    #[test]
    fn probing_the_real_driver_is_safe_and_consistent() {
        let found = available();
        let list = gpus();
        if found {
            assert!(!list.is_empty(), "NVML initialised but reported no devices");
            for g in &list {
                assert!(g.total > 0, "{g:?}");
                assert!(g.used <= g.total, "{g:?}");
                assert!((0.0..=100.0).contains(&g.percent_used()), "{g:?}");
            }
            assert!(room_for_local_model().is_some());
        } else {
            assert!(list.is_empty());
            assert_eq!(room_for_local_model(), None, "unknown, not false");
        }
    }

    #[test]
    fn per_process_vram_is_ordered_largest_first_and_never_panics() {
        let procs = processes(0);
        for w in procs.windows(2) {
            assert!(w[0].used >= w[1].used);
        }
    }

    /// A process using the GPU for both rendering and compute appears in both of
    /// NVML's lists, with its whole footprint in each. Listing it twice doubles
    /// its memory — observed with Brave and cosmic-files on this machine.
    #[test]
    fn a_process_in_both_nvml_lists_is_counted_once() {
        let procs = processes(0);
        let mut pids: Vec<u32> = procs.iter().map(|p| p.pid).collect();
        let before = pids.len();
        pids.sort_unstable();
        pids.dedup();
        assert_eq!(pids.len(), before, "a pid was listed more than once");
    }

    /// Attributed VRAM cannot exceed what the device says is in use. If
    /// deduplication were wrong, this is where the double-count would show.
    #[test]
    fn attributed_memory_does_not_exceed_what_the_device_reports() {
        if let Some(g) = primary() {
            let attributed = attributed(g.index);
            assert!(
                attributed <= g.total,
                "attributed {attributed} exceeds the card's {} bytes",
                g.total
            );
        }
    }
}
