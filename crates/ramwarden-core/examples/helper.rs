//! Exercise the privileged helper against a real process.
use ramwarden_core::helper;
use ramwarden_kernel::{Root, smaps};

fn main() {
    println!("helper socket: {}", helper::socket_path().display());
    println!("helper present: {}", helper::present());
    println!("daemon holds CAP_SYS_NICE: {}", ramwarden_kernel::madvise::has_cap_sys_nice());

    // A victim holding real, cold anonymous memory.
    let mut child = std::process::Command::new("python3")
        .arg("-c")
        .arg("import ctypes,time; ctypes.CDLL('libc.so.6').prctl(15,b'rw-victim',0,0,0); \
              buf=bytearray(300*1024*1024); \
              [buf.__setitem__(i,1) for i in range(0,len(buf),4096)]; time.sleep(120)")
        .spawn()
        .expect("spawn victim");
    std::thread::sleep(std::time::Duration::from_secs(4));
    let pid = child.id() as i32;
    let root = Root::system();

    let before = smaps::rollup(&root, pid).map(|r| r.rss).unwrap_or(0);
    let vmas = smaps::cold_vmas(&root, pid, 1.0, 2 * 1024 * 1024).unwrap_or_default();
    println!("\nvictim pid {pid}: RSS {:.0} MB, {} candidate region(s) totalling {:.0} MB",
        before as f64 / 1e6, vmas.len(),
        vmas.iter().map(|v| v.len()).sum::<u64>() as f64 / 1e6);

    let outcome = helper::page_out(pid, &vmas);
    println!("\nhelper page_out ->");
    println!("  bytes_freed: {:.0} MB", outcome.bytes_freed as f64 / 1e6);
    println!("  notes:       {:?}", outcome.notes);

    std::thread::sleep(std::time::Duration::from_millis(400));
    let after = smaps::rollup(&root, pid).map(|r| r.rss).unwrap_or(0);
    println!("  victim RSS:  {:.0} MB -> {:.0} MB", before as f64/1e6, after as f64/1e6);
    println!("  still alive: {}", std::fs::metadata(format!("/proc/{pid}")).is_ok());

    child.kill().ok();
    child.wait().ok();
}
