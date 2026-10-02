use ramwarden_ai::vram;
fn main() {
    println!("NVML available: {}", vram::available());
    for g in vram::gpus() {
        println!("  GPU{} {} — {:.0}/{:.0} MB used ({:.1}%), {}% busy",
            g.index, g.name, g.used as f64/1e6, g.total as f64/1e6, g.percent_used(), g.utilisation);
        println!("    room for nemotron-3-nano:4b (+{:.1} GB margin): {}",
            vram::MODEL_MARGIN as f64/1e9, g.has_room_for(vram::NEMOTRON_4B_BYTES, vram::MODEL_MARGIN));
        for p in vram::processes(g.index).iter().take(8) {
            let comm = std::fs::read_to_string(format!("/proc/{}/comm", p.pid))
                .unwrap_or_default().trim().to_string();
            println!("    pid {:<8} {:<20} {:>7.0} MB", p.pid, comm, p.used as f64/1e6);
        }
    }
}
