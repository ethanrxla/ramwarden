//! macOS backend: libproc, sysctl and mach in place of `/proc` and `/sys`.
//!
//! The public functions in `process`, `meminfo`, `smaps` and `psi` keep their
//! Linux signatures and dispatch here on macOS, so `ramwarden-core` and the
//! daemon need no `cfg`. Anything macOS cannot do (cgroup reclaim, pidfd,
//! `process_madvise`, zram) reports [`Error::Unsupported`] instead.
//!
//! # Mapping
//!
//! * **PSS** -> `ri_phys_footprint` from `proc_pid_rusage`: the figure Activity
//!   Monitor shows as "Memory". It counts compressed pages and does not
//!   double-count shared mappings the way summed RSS does.
//! * **PSI** -> a proxy built from the kernel's own pressure level
//!   (`kern.memorystatus_vm_pressure_level`: 1 normal, 2 warn, 4 critical) and
//!   percent of memory available (`kern.memorystatus_level`). macOS has no stall
//!   accounting, so the numbers are synthetic — chosen to land in the ladder's
//!   existing rungs (warn -> tab unload, critical -> suspend), and deliberately
//!   short of the kill rung.

// libc points at mach2 for the few mach calls here; they do not justify a new dependency.
#![allow(deprecated)]

use std::ffi::{CString, c_char, c_int, c_void};
use std::mem::{MaybeUninit, size_of};
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::meminfo::Memory;
use crate::process::ProcEntry;
use crate::psi::{Psi, Window};
use crate::smaps::Rollup;
use crate::{Error, Result};

// ── sysctl ──────────────────────────────────────────────────────────────────

fn sysctl_raw(name: &str, buf: *mut c_void, len: &mut usize) -> bool {
    let Ok(c) = CString::new(name) else { return false };
    // SAFETY: `buf` points to `*len` writable bytes owned by the caller.
    unsafe { libc::sysctlbyname(c.as_ptr(), buf, len, std::ptr::null_mut(), 0) == 0 }
}

/// An integer sysctl, whether the kernel reports it as 4 or 8 bytes.
fn sysctl_int(name: &str) -> Option<u64> {
    let mut buf = [0u8; 8];
    let mut len = buf.len();
    if !sysctl_raw(name, buf.as_mut_ptr().cast(), &mut len) {
        return None;
    }
    match len {
        4 => Some(u32::from_ne_bytes(buf[..4].try_into().ok()?) as u64),
        8 => Some(u64::from_ne_bytes(buf)),
        _ => None,
    }
}

fn sysctl_struct<T: Copy>(name: &str) -> Option<T> {
    let mut out = MaybeUninit::<T>::zeroed();
    let mut len = size_of::<T>();
    if !sysctl_raw(name, out.as_mut_ptr().cast(), &mut len) || len != size_of::<T>() {
        return None;
    }
    // SAFETY: the kernel filled exactly size_of::<T>() bytes of a plain C struct.
    Some(unsafe { out.assume_init() })
}

fn page_size() -> u64 {
    crate::process::page_size()
}

// ── memory ──────────────────────────────────────────────────────────────────

fn vm_stats() -> Option<libc::vm_statistics64> {
    let mut s = MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `s` is large enough for HOST_VM_INFO64 and `count` is its size in
    // 32-bit words, as host_statistics64 requires.
    let rc = unsafe {
        libc::host_statistics64(
            libc::mach_host_self(),
            libc::HOST_VM_INFO64,
            s.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if rc != libc::KERN_SUCCESS {
        return None;
    }
    // SAFETY: success means the struct was filled.
    Some(unsafe { s.assume_init() })
}

pub(crate) fn meminfo() -> Result<Memory> {
    let total = sysctl_int("hw.memsize").ok_or(Error::Unsupported("hw.memsize"))?;
    let vm = vm_stats().ok_or(Error::Unsupported("host_statistics64"))?;
    let ps = page_size();

    let free = vm.free_count as u64 * ps;
    // Free, inactive and speculative pages can be handed out without swapping;
    // active, wired and compressed pages cannot.
    let available = (vm.free_count as u64 + vm.inactive_count as u64 + vm.speculative_count as u64) * ps;
    let swap = sysctl_struct::<libc::xsw_usage>("vm.swapusage");

    Ok(Memory {
        total,
        free,
        available: available.min(total),
        buffers: 0,
        cached: vm.external_page_count as u64 * ps,
        swap_total: swap.map(|s| s.xsu_total).unwrap_or(0),
        swap_free: swap.map(|s| s.xsu_avail).unwrap_or(0),
        swap_cached: 0,
    })
}

/// Pure mapping from the kernel's pressure signals to the ladder's scale.
///
/// `level` is 1 normal / 2 warn / 4 critical; `avail_pct` is percent of memory
/// available (0-100).
pub(crate) fn proxy_psi(level: u64, avail_pct: u64) -> Psi {
    let (some, full) = match level {
        4.. => (16.0, 10.0),
        2..=3 => (11.0, 0.0),
        // Normal: drift upward as free memory shrinks, so the first rungs
        // (which are advisory here) can still be observed in the action log.
        _ => ((30.0 - avail_pct.min(100) as f64).max(0.0) * 0.1, 0.0),
    };
    let w = |v: f64| Window { avg10: v, avg60: v, avg300: v, total_us: 0 };
    Psi { some: w(some), full: w(full) }
}

pub(crate) fn pressure() -> Result<Psi> {
    let level = sysctl_int("kern.memorystatus_vm_pressure_level")
        .ok_or(Error::Unsupported("kern.memorystatus_vm_pressure_level"))?;
    let avail = sysctl_int("kern.memorystatus_level").unwrap_or(100);
    Ok(proxy_psi(level, avail))
}

// ── processes ───────────────────────────────────────────────────────────────

fn missing(pid: i32) -> Error {
    Error::io(
        format!("pid {pid}"),
        std::io::Error::from(std::io::ErrorKind::NotFound),
    )
}

fn denied(pid: i32) -> Error {
    Error::Denied { path: format!("pid {pid}").into(), hint: "process belongs to another user" }
}

pub(crate) fn bsd_info(pid: i32) -> Option<libc::proc_bsdinfo> {
    let mut i = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let sz = size_of::<libc::proc_bsdinfo>();
    // SAFETY: buffer is `sz` writable bytes.
    let r = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, i.as_mut_ptr().cast(), sz as c_int) };
    // SAFETY: a full-size return means the struct is initialised.
    (r as usize == sz).then(|| unsafe { i.assume_init() })
}

fn task_info(pid: i32) -> Option<libc::proc_taskinfo> {
    let mut i = MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let sz = size_of::<libc::proc_taskinfo>();
    // SAFETY: as above.
    let r = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, i.as_mut_ptr().cast(), sz as c_int) };
    // SAFETY: as above.
    (r as usize == sz).then(|| unsafe { i.assume_init() })
}

fn cstr(bytes: &[c_char]) -> String {
    let b: Vec<u8> = bytes.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&b).into_owned()
}

/// Mach absolute time -> nanoseconds. On Apple silicon the ratio is 125/3.
fn abs_to_ns(abs: u64) -> u64 {
    static TB: OnceLock<(u64, u64)> = OnceLock::new();
    let (n, d) = *TB.get_or_init(|| {
        let mut info = libc::mach_timebase_info { numer: 1, denom: 1 };
        // SAFETY: `info` is a valid out-pointer.
        unsafe { libc::mach_timebase_info(&mut info) };
        (info.numer.max(1) as u64, info.denom.max(1) as u64)
    });
    (abs as u128 * n as u128 / d as u128) as u64
}

pub(crate) fn boot_time() -> Result<u64> {
    sysctl_struct::<libc::timeval>("kern.boottime")
        .map(|t| t.tv_sec as u64)
        .ok_or(Error::Unsupported("kern.boottime"))
}

pub(crate) fn entry(pid: i32) -> Result<ProcEntry> {
    let b = bsd_info(pid).ok_or_else(|| missing(pid))?;
    let name = cstr(&b.pbi_name);
    let comm = if name.is_empty() { cstr(&b.pbi_comm) } else { name };

    // SIDL=1 SRUN=2 SSLEEP=3 SSTOP=4 SZOMB=5. The BSD layer reports SRUN for
    // nearly every live process (scheduling state lives on Mach threads), so
    // running and sleeping are not distinguishable here; both map to 'S'. Only
    // stopped and zombie matter to the ladder.
    let state = match b.pbi_status {
        1 => 'I',
        4 => 'T',
        5 => 'Z',
        _ => 'S',
    };

    let boot = boot_time().unwrap_or(0);
    let hz = crate::process::clock_ticks();
    let since_boot = b.pbi_start_tvsec.saturating_sub(boot);
    let start_ticks = since_boot * hz + b.pbi_start_tvusec * hz / 1_000_000;

    // taskinfo is refused for other users' processes; the row is still useful.
    let (cpu_ticks, rss_pages) = match task_info(pid) {
        Some(t) => (
            abs_to_ns(t.pti_total_user + t.pti_total_system) / (1_000_000_000 / hz),
            t.pti_resident_size / page_size(),
        ),
        None => (0, 0),
    };

    Ok(ProcEntry {
        pid,
        comm,
        state,
        ppid: b.pbi_ppid as i32,
        session: b.pbi_pgid as i32,
        tty_nr: if b.e_tdev == 0 || b.e_tdev == u32::MAX { 0 } else { b.e_tdev as i32 | 1 },
        cpu_ticks,
        start_ticks,
        rss_pages,
    })
}

pub(crate) fn table() -> Result<Vec<ProcEntry>> {
    // SAFETY: a null buffer asks only for the count.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Err(Error::Unsupported("proc_listallpids"));
    }
    let mut pids = vec![0i32; n as usize + 64];
    // SAFETY: buffer size is given in bytes and matches the allocation.
    let got = unsafe {
        libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * size_of::<i32>()) as c_int)
    };
    if got <= 0 {
        return Err(Error::Unsupported("proc_listallpids"));
    }
    pids.truncate(got as usize);

    let mut out: Vec<ProcEntry> = pids
        .into_iter()
        .filter(|&p| p > 0)
        .filter_map(|p| entry(p).ok())
        .collect();
    out.sort_by_key(|p| p.pid);
    Ok(out)
}

pub(crate) fn exe(pid: i32) -> Result<PathBuf> {
    let mut buf = vec![0u8; 4096];
    // SAFETY: buffer length is passed alongside the pointer.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return Err(if bsd_info(pid).is_some() { denied(pid) } else { missing(pid) });
    }
    buf.truncate(n as usize);
    Ok(PathBuf::from(String::from_utf8_lossy(&buf).into_owned()))
}

pub(crate) fn uid(pid: i32) -> Result<u32> {
    bsd_info(pid).map(|b| b.pbi_ruid).ok_or_else(|| missing(pid))
}

/// Full argv via `KERN_PROCARGS2`, falling back to the executable path.
pub(crate) fn cmdline(pid: i32) -> Result<String> {
    let argmax = sysctl_int("kern.argmax").unwrap_or(262_144) as usize;
    let mut buf = vec![0u8; argmax];
    let mut len = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: mib has 3 entries; buf/len describe a writable buffer.
    let rc = unsafe {
        libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0)
    };
    if rc != 0 || len < 4 {
        return exe(pid).map(|p| p.to_string_lossy().into_owned());
    }
    buf.truncate(len);
    Ok(parse_procargs(&buf))
}

/// `argc` (i32), exec path, NUL padding, then `argc` NUL-separated arguments.
pub(crate) fn parse_procargs(buf: &[u8]) -> String {
    if buf.len() < 4 {
        return String::new();
    }
    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]).max(0) as usize;
    let rest = &buf[4..];
    let path_end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    let after = &rest[path_end..];
    let start = after.iter().position(|&b| b != 0).unwrap_or(after.len());
    after[start..]
        .split(|&b| b == 0)
        .take(argc)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn rollup(pid: i32) -> Result<Rollup> {
    let mut ri = MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: proc_pid_rusage fills a rusage_info_v4 for flavor V4.
    let rc = unsafe {
        libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, ri.as_mut_ptr().cast())
    };
    if rc != 0 {
        return Err(if bsd_info(pid).is_some() { denied(pid) } else { missing(pid) });
    }
    // SAFETY: rc == 0 means filled.
    let ri = unsafe { ri.assume_init() };
    let footprint = ri.ri_phys_footprint;
    Ok(Rollup {
        rss: ri.ri_resident_size,
        pss: footprint,
        pss_dirty: footprint,
        pss_anon: footprint,
        // No per-page reference bits on macOS, so nothing is provably cold.
        referenced: ri.ri_resident_size,
        locked: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_psi_levels_land_in_expected_rungs() {
        assert_eq!(proxy_psi(1, 80).some.avg10, 0.0);
        assert!(proxy_psi(1, 5).some.avg10 > 2.0, "low free memory nudges the first rung");
        assert_eq!(proxy_psi(2, 50).some.avg10, 11.0);
        let crit = proxy_psi(4, 5);
        assert!(crit.some.avg10 > 15.0 && crit.full.avg10 < 25.0, "critical suspends but never kills");
    }

    #[test]
    fn procargs_parse() {
        let mut b = 2i32.to_ne_bytes().to_vec();
        b.extend_from_slice(b"/bin/ls\0\0\0ls\0-l\0PATH=/bin\0");
        assert_eq!(parse_procargs(&b), "ls -l");
    }

    #[test]
    fn sees_ourselves() {
        let me = std::process::id() as i32;
        let e = entry(me).expect("own process");
        assert_eq!(e.pid, me);
        assert!(table().unwrap().iter().any(|p| p.pid == me));
        assert!(rollup(me).unwrap().pss > 0);
        assert!(cmdline(me).unwrap().contains("ramwarden"));
        assert_eq!(uid(me).unwrap(), unsafe { libc::getuid() });
    }

    #[test]
    fn memory_matches_hardware() {
        let m = meminfo().unwrap();
        assert_eq!(Some(m.total), sysctl_int("hw.memsize"));
        assert!(m.available > 0 && m.available <= m.total);
        assert!(boot_time().unwrap() > 1_600_000_000);
    }

    #[test]
    fn pressure_reads() {
        let p = pressure().unwrap();
        assert!(p.some.avg10 >= 0.0);
    }
}
