//! Thin, safe wrappers over sysctl and libproc.

use std::ffi::{CString, c_char, c_int, c_void};
use std::io::Read;
use std::mem::{MaybeUninit, size_of};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use std::sync::OnceLock;

pub fn sysctl_raw(name: &str, buf: *mut c_void, len: &mut usize) -> bool {
    let Ok(c) = CString::new(name) else { return false };
    // SAFETY: `buf` points to `*len` writable bytes owned by the caller.
    unsafe { libc::sysctlbyname(c.as_ptr(), buf, len, std::ptr::null_mut(), 0) == 0 }
}

/// An integer sysctl, whether the kernel reports it as 4 or 8 bytes.
pub fn sysctl_int(name: &str) -> Option<u64> {
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

pub fn sysctl_struct<T: Copy>(name: &str) -> Option<T> {
    let mut out = MaybeUninit::<T>::zeroed();
    let mut len = size_of::<T>();
    if !sysctl_raw(name, out.as_mut_ptr().cast(), &mut len) || len != size_of::<T>() {
        return None;
    }
    // SAFETY: the kernel filled exactly size_of::<T>() bytes of a plain C struct.
    Some(unsafe { out.assume_init() })
}

pub fn page_size() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if p > 0 { p as u64 } else { 16384 }
}

pub fn vm_stats() -> Option<libc::vm_statistics64> {
    let mut s = MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `s` fits HOST_VM_INFO64 and `count` is its size in 32-bit words.
    let rc = unsafe {
        libc::host_statistics64(libc::mach_host_self(), libc::HOST_VM_INFO64, s.as_mut_ptr().cast(), &mut count)
    };
    // SAFETY: success means the struct was filled.
    (rc == libc::KERN_SUCCESS).then(|| unsafe { s.assume_init() })
}

pub fn bsd_info(pid: i32) -> Option<libc::proc_bsdinfo> {
    let mut i = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let sz = size_of::<libc::proc_bsdinfo>();
    // SAFETY: buffer is `sz` writable bytes.
    let r = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, i.as_mut_ptr().cast(), sz as c_int) };
    // SAFETY: a full-size return means the struct is initialised.
    (r as usize == sz).then(|| unsafe { i.assume_init() })
}

pub fn task_info(pid: i32) -> Option<libc::proc_taskinfo> {
    let mut i = MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let sz = size_of::<libc::proc_taskinfo>();
    // SAFETY: as above.
    let r = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, i.as_mut_ptr().cast(), sz as c_int) };
    // SAFETY: as above.
    (r as usize == sz).then(|| unsafe { i.assume_init() })
}

/// `ri_phys_footprint`: what Activity Monitor calls a process's Memory.
pub fn footprint(pid: i32) -> Option<u64> {
    let mut ri = MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: proc_pid_rusage fills a rusage_info_v4 for flavor V4.
    let rc = unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, ri.as_mut_ptr().cast()) };
    // SAFETY: rc == 0 means filled.
    (rc == 0).then(|| unsafe { ri.assume_init() }.ri_phys_footprint)
}

pub fn cstr(bytes: &[c_char]) -> String {
    let b: Vec<u8> = bytes.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&b).into_owned()
}

/// Mach absolute time -> nanoseconds. On Apple silicon the ratio is 125/3.
pub fn abs_to_ns(abs: u64) -> u64 {
    static TB: OnceLock<(u64, u64)> = OnceLock::new();
    let (n, d) = *TB.get_or_init(|| {
        let mut info = libc::mach_timebase_info { numer: 1, denom: 1 };
        // SAFETY: `info` is a valid out-pointer.
        unsafe { libc::mach_timebase_info(&mut info) };
        (info.numer.max(1) as u64, info.denom.max(1) as u64)
    });
    (abs as u128 * n as u128 / d as u128) as u64
}

pub fn all_pids() -> Vec<i32> {
    // SAFETY: a null buffer asks only for the count.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0i32; n as usize + 64];
    // SAFETY: buffer size is given in bytes and matches the allocation.
    let got = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * size_of::<i32>()) as c_int) };
    pids.truncate(got.max(0) as usize);
    pids.retain(|&p| p > 0);
    pids
}

/// Full argv via `KERN_PROCARGS2`, reusing `buf` across calls.
pub fn cmdline(pid: i32, buf: &mut Vec<u8>) -> String {
    let argmax = sysctl_int("kern.argmax").unwrap_or(262_144) as usize;
    buf.resize(argmax, 0);
    let mut len = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: mib has 3 entries; buf/len describe a writable buffer.
    let rc = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
    if rc != 0 || len < 4 {
        return String::new();
    }
    parse_procargs(&buf[..len])
}

/// `argc` (i32), exec path, NUL padding, then `argc` NUL-separated arguments.
pub fn parse_procargs(buf: &[u8]) -> String {
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

/// Run a command and return its stdout, or `None` if it fails or exceeds `timeout`.
///
/// Never blocks past the timeout: `lsappinfo` and friends ask other processes
/// questions, and a process RamWarden itself froze cannot answer. A hung probe
/// would otherwise stall the whole daemon. The reader runs on its own thread so
/// output larger than a pipe buffer cannot block the child, and a timed-out child
/// is killed.
pub fn run_timeout(cmd: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(cmd).args(args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut out = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    match rx.recv_timeout(timeout) {
        Ok(buf) => {
            let _ = child.wait();
            Some(String::from_utf8_lossy(&buf).into_owned())
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            tracing::warn!("{cmd} did not answer within {timeout:?}; killed it");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_output_and_enforces_the_timeout() {
        assert_eq!(run_timeout("echo", &["hi"], Duration::from_secs(2)).as_deref(), Some("hi\n"));
        let t = std::time::Instant::now();
        assert_eq!(run_timeout("sleep", &["30"], Duration::from_millis(200)), None);
        assert!(t.elapsed() < Duration::from_secs(2), "must not wait for the child");
        assert_eq!(run_timeout("definitely-not-a-command", &[], Duration::from_secs(1)), None);
    }

    #[test]
    fn large_output_does_not_deadlock() {
        // Far more than a 64 KiB pipe buffer.
        let out = run_timeout("sh", &["-c", "head -c 500000 /dev/zero | tr '\\0' 'a'"], Duration::from_secs(5)).unwrap();
        assert_eq!(out.len(), 500_000);
    }
}
