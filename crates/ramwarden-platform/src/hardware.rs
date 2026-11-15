//! The machine's own hardware: CPU model and core counts.
//!
//! The warden's chat is asked "how many CPU cores do I have?" (2026-11-15),
//! and the answer is not in any memory reading. Each mechanism crate reads
//! it the way its OS reports it: `/proc/cpuinfo` on Linux, the processor
//! topology API on Windows, `sysctl` on macOS.

use crate::{Error, Result};

/// What the machine is built from. Every field is optional: a source fills in
/// what its OS reports and leaves the rest unknown rather than guessing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hardware {
    /// The CPU's marketing name, e.g. `AMD Ryzen 7 9700X 8-Core Processor`.
    pub cpu_model: Option<String>,
    /// Physical cores, across every socket.
    pub physical_cores: Option<u32>,
    /// Hardware threads the OS schedules on (cores × SMT).
    pub logical_cores: Option<u32>,
    /// Performance and efficiency cores, where the CPU has both (Apple
    /// silicon, Intel hybrid). `None` when the OS does not say.
    pub performance_cores: Option<u32>,
    pub efficiency_cores: Option<u32>,
}

/// Reads [`Hardware`]. Cheap enough to call per question; nothing caches it.
pub trait HardwareSource: Send + Sync {
    fn read(&self) -> Result<Hardware>;
}

/// For platforms (and tests) with no hardware reading.
pub struct NoHardware;

impl HardwareSource for NoHardware {
    fn read(&self) -> Result<Hardware> {
        Err(Error::Unsupported("hardware"))
    }
}

/// Logical processors the OS schedules on, from the standard library. Every
/// source can fall back to this.
pub fn logical_cores() -> Option<u32> {
    std::thread::available_parallelism().ok().map(|n| n.get() as u32)
}

/// The CPU's brand string from CPUID leaves 0x80000002..4, on x86_64. Not an
/// OS call, so the Windows and Linux sources share it. `None` elsewhere.
pub fn cpu_brand() -> Option<String> {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::__cpuid;
        // CPUID exists on every x86_64 CPU (the intrinsic is safe to call);
        // the extended leaf count is checked before the brand leaves are read.
        let max = __cpuid(0x8000_0000).eax;
        if max < 0x8000_0004 {
            return None;
        }
        let mut bytes = Vec::with_capacity(48);
        for leaf in 0x8000_0002u32..=0x8000_0004 {
            let r = __cpuid(leaf);
            for word in [r.eax, r.ebx, r.ecx, r.edx] {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
        }
        let text = String::from_utf8_lossy(&bytes).trim_matches(char::from(0)).trim().to_string();
        (!text.is_empty()).then_some(text)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_fallbacks_answer_on_this_machine() {
        assert!(logical_cores().is_some_and(|n| n >= 1));
        #[cfg(target_arch = "x86_64")]
        assert!(cpu_brand().is_some_and(|b| !b.is_empty()));
        assert!(NoHardware.read().is_err());
    }
}
