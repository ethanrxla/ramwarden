//! Finding the GPU worker.
//!
//! `ramwarden-gpu` is installed beside the daemon when it was built (with
//! `--features cuda,embed`). The daemon never loads CUDA itself — a CUDA build
//! of the daemon held 640 MB private for as long as it ran — it starts the
//! worker when there is GPU work, and the worker exits when it is done.

use std::path::PathBuf;

/// The worker's path, if it is installed and an NVIDIA GPU is present.
///
/// The GPU check is NVML, which the daemon already uses for VRAM accounting
/// and which creates no CUDA context.
pub fn gpu_worker() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "ramwarden-gpu.exe" } else { "ramwarden-gpu" };
    let path = std::env::current_exe().ok()?.with_file_name(exe);
    (path.exists() && ramwarden_ai::vram::available()).then_some(path)
}
