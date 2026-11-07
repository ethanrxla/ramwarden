//! What one monitor tick costs: wall time, heap allocations and bytes allocated
//! for the process sample and for the ladder's candidate selection.
//!
//! Run with: cargo run --release -p ramwarden-core --example tick_cost
//!
//! The process half (Linux only) builds a synthetic `/proc` shaped like a busy
//! desktop: 400 processes, 60 of them browser-sized with long Chromium command
//! lines and a `smaps_rollup`, 30 file descriptors each, and a TCP table whose
//! sockets have to be traced back to their owners through those descriptors.
//! Synthetic files read faster than real procfs ones (the kernel walks every
//! mapping to produce a real `smaps_rollup`), so the time saved on a real
//! machine is larger than the time saved here. The allocation counts are exact.
//!
//! The ladder half runs [`Ladder::step`] at the kill rung over 400 preloaded
//! processes and a 12-entry watchlist, with a judge that keeps everything, so
//! each step builds both candidate lists and asks twice without acting.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use ramwarden_core::actuator::Actuator;
use ramwarden_core::config;
use ramwarden_core::detector::Detector;
use ramwarden_core::judge::{Candidate, Intent, Judge, Judgement};
use ramwarden_core::ladder::{Ladder, World};
use ramwarden_core::roles::classify;
use ramwarden_core::signals::Signals;
use ramwarden_platform::{Pressure, fake};

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards to the system allocator unchanged, only counting.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(l.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(n as u64, Ordering::Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Read syscalls this process has made so far, from `/proc/self/io` (Linux).
fn read_syscalls() -> u64 {
    std::fs::read_to_string("/proc/self/io")
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("syscr: ")?.trim().parse().ok()))
        .unwrap_or(0)
}

/// Run `f` `n` times and print the mean cost of one run.
fn measure(label: &str, n: u32, mut f: impl FnMut()) {
    f(); // warm caches and the first-sample path
    f();
    let (a0, b0) = (ALLOCS.load(Ordering::Relaxed), BYTES.load(Ordering::Relaxed));
    let r0 = read_syscalls();
    let t = Instant::now();
    for _ in 0..n {
        f();
    }
    let per = t.elapsed() / n;
    // Less the one read of /proc/self/io itself.
    let reads = read_syscalls().saturating_sub(r0 + 1) / n as u64;
    let allocs = (ALLOCS.load(Ordering::Relaxed) - a0) / n as u64;
    let bytes = (BYTES.load(Ordering::Relaxed) - b0) / n as u64;
    println!(
        "{label:<32} {per:>9.2?}/tick {allocs:>7} allocs {:>8.1} KB alloc'd {reads:>6} reads",
        bytes as f64 / 1024.0
    );
}

struct KeepAll(u32);

impl Judge for KeepAll {
    fn judge(&mut self, _: Intent, c: &[Candidate]) -> Result<Vec<Judgement>, String> {
        self.0 += 1;
        Ok(c.iter()
            .map(|c| Judgement { name: c.name.clone(), close: false, confidence: 0.0, reason: "bench".into() })
            .collect())
    }
}

fn ladder_cost() {
    let mut sigs = Vec::new();
    for i in 0..400 {
        let name = format!("app{}", i % 80);
        let mut s = Signals::new(1000 + i, &name, classify(&name, "", None));
        s.pss = if i < 80 { 300 << 20 } else { 8 << 20 };
        s.age_minutes = 600.0;
        sigs.push(s);
    }
    let det = Detector::preloaded(true, sigs);
    let watchlist: Vec<String> = (0..12).map(|i| format!("app{i}")).collect();
    let world = std::sync::Arc::new(std::sync::Mutex::new(fake::World::default()));
    let platform = fake::platform(world);
    let mut ladder = Ladder::new(config::Ladder::default(), Actuator::for_platform(&platform));
    let mut judge = KeepAll(0);
    measure("ladder step, kill rung, judged", 2000, || {
        let mut w = World {
            det: &det,
            groups: None,
            psi: Pressure { some_avg10: 90.0, full_avg10: 90.0, ..Default::default() },
            available_bytes: u64::MAX,
            watchlist: &watchlist,
            goal: String::new(),
            tabs: None,
            judge: Some(&mut judge),
        };
        std::hint::black_box(ladder.step(&mut w));
    });
}

#[cfg(target_os = "linux")]
fn sample_cost() {
    use std::fs;
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path();
    fs::create_dir_all(p.join("proc/net")).unwrap();
    fs::write(p.join("proc/stat"), "cpu 0 0 0 0\nbtime 1700000000\n").unwrap();

    let mut tcp = String::from("  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n");
    let mut inode = 50_000u64;
    let chromium_args = "--type=renderer --enable-features=".to_string() + &"SomeFeature,".repeat(100);
    for i in 0..400i32 {
        let pid = 2000 + i;
        let d = p.join(format!("proc/{pid}"));
        fs::create_dir_all(d.join("fd")).unwrap();
        let big = i < 60;
        let rss_pages = if big { 60_000 } else { 1_000 };
        let comm = if big { "chrome".to_string() } else { format!("svc{}", i % 50) };
        fs::write(
            d.join("stat"),
            format!("{pid} ({comm}) S 1 {pid} {pid} 0 -1 4194560 0 0 0 0 {} 10 0 0 20 0 1 0 5000 1000000 {rss_pages} 0\n", 100 + i),
        )
        .unwrap();
        let cmd = if big { format!("/opt/google/chrome/chrome\0{chromium_args}") } else { format!("/usr/bin/{comm}\0--daemon") };
        fs::write(d.join("cmdline"), cmd).unwrap();
        fs::write(d.join("status"), "Name:\tx\nUid:\t1000\t1000\t1000\t1000\n").unwrap();
        if big {
            fs::write(
                d.join("smaps_rollup"),
                "00400000-7fff [rollup]\nRss: 240000 kB\nPss: 120000 kB\nPss_Dirty: 1 kB\nPss_Anon: 1 kB\nReferenced: 1 kB\nLocked: 0 kB\n",
            )
            .unwrap();
        }
        for fd in 0..30 {
            let target = if fd % 10 == 9 {
                inode += 1;
                let state = if fd == 9 && i % 40 == 0 { "0A" } else { "01" };
                tcp.push_str(&format!(
                    "   0: 0100007F:{:04X} 0100007F:1E93 {state} 00000000:00000000 00:00000000 00000000  1000        0 {inode} 1\n",
                    3000 + i
                ));
                format!("socket:[{inode}]")
            } else {
                format!("/tmp/file{fd}")
            };
            symlink(target, d.join(format!("fd/{fd}"))).unwrap();
        }
    }
    fs::write(p.join("proc/net/tcp"), tcp).unwrap();

    let root = ramwarden_kernel::Root::at(p);
    let mut det = Detector::linux(root.clone());
    let desktop = Default::default();
    measure("process sample, 400 processes", 200, || det.sample(&desktop).unwrap());
    // What the sample used to pay every tick and now pays once per
    // `sampler::TABLES_EVERY`: tracing socket owners through every descriptor.
    measure("socket table read", 200, || {
        std::hint::black_box(ramwarden_kernel::net::sockets(&root).unwrap());
    });
}

#[cfg(not(target_os = "linux"))]
fn sample_cost() {
    println!("process sample: the synthetic /proc half runs on Linux only");
}

fn main() {
    sample_cost();
    ladder_cost();
}
