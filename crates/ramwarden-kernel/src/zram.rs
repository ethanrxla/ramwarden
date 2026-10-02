//! zram: how much of "memory used" is already compressed away.
//!
//! # Why RamWarden must read this
//!
//! On the machine this was written for, `free` reports ~23 GiB of 30 GiB used
//! and RamWarden v1 concluded the machine was in trouble. It was not. zram held
//! 5.21 GB of anonymous pages in 1.17 GB of actual RAM — a 4.5x saving, so
//! roughly 4 GB of that "used" figure was memory the kernel had already
//! reclaimed. Acting on percent-used without reading zram means nagging the user
//! about pressure that does not exist.
//!
//! It is also the meter for the reclaim lever: when a `memory.reclaim` write
//! succeeds, the freed anonymous pages land here, so a rise in `orig_data_size`
//! is independent confirmation that reclaim did something real.

use std::fs;
use std::path::PathBuf;

use crate::{Error, Result, Root};

/// One zram device's `mm_stat`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Uncompressed size of everything stored — the memory the system would be
    /// using if zram were not there.
    pub orig_data_size: u64,
    /// Size after compression, excluding allocator overhead.
    pub compr_data_size: u64,
    /// Actual RAM the device occupies, including allocator overhead. This is the
    /// honest cost, and the figure to subtract when reasoning about headroom.
    pub mem_used_total: u64,
    pub mem_used_max: u64,
    /// Pages that were all one value (usually zero) and cost no storage at all.
    pub same_pages: u64,
    pub huge_pages: u64,
}

impl Stats {
    /// How well the data is compressing. 4.5 is typical for zstd on desktop
    /// anonymous memory; 1.0 would mean zram is buying nothing.
    pub fn ratio(&self) -> f64 {
        if self.mem_used_total == 0 {
            return 1.0;
        }
        self.orig_data_size as f64 / self.mem_used_total as f64
    }

    /// RAM that zram is saving right now — the amount by which a naive
    /// "used memory" reading overstates real pressure.
    pub fn saved(&self) -> u64 {
        self.orig_data_size.saturating_sub(self.mem_used_total)
    }
}

/// A zram block device.
#[derive(Clone, Debug)]
pub struct Device {
    path: PathBuf,
    name: String,
}

impl Device {
    /// Every zram device the kernel exposes, in device order.
    ///
    /// Returns an empty vector when zram is not configured; that is a normal
    /// system, not an error, and callers simply lose the correction.
    pub fn discover(root: &Root) -> Result<Vec<Device>> {
        let block = root.join("sys/block");
        let entries = match fs::read_dir(&block) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(&block, e)),
        };

        let mut out: Vec<Device> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with("zram").then(|| Device {
                    path: e.path(),
                    name,
                })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn stats(&self) -> Result<Stats> {
        let path = self.path.join("mm_stat");
        let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;

        let f: Vec<u64> = text
            .split_whitespace()
            .map(|v| v.parse().unwrap_or(0))
            .collect();

        // The kernel has appended fields over time (9 on 7.1, 8 before
        // `huge_pages_since`, 7 before that). Require only the prefix we read so
        // a newer kernel adding a tenth column does not break the parse.
        if f.len() < 7 {
            return Err(Error::parse(
                &path,
                format!("expected at least 7 fields, got {}", f.len()),
            ));
        }

        Ok(Stats {
            orig_data_size: f[0],
            compr_data_size: f[1],
            mem_used_total: f[2],
            mem_used_max: f[4],
            same_pages: f[5],
            huge_pages: f[7.min(f.len() - 1)],
        })
    }

    /// Configured backing size. Not a memory cost — zram only allocates what it
    /// actually stores, so treating `disksize` as usage badly overstates it.
    pub fn disksize(&self) -> Result<u64> {
        let path = self.path.join("disksize");
        let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        text.trim()
            .parse()
            .map_err(|_| Error::parse(&path, "disksize is not an integer"))
    }

    /// The active compression algorithm, e.g. `zstd`. The kernel marks it with
    /// brackets in a list of what is available.
    pub fn algorithm(&self) -> Result<String> {
        let path = self.path.join("comp_algorithm");
        let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        Ok(text
            .split_whitespace()
            .find_map(|w| w.strip_prefix('[')?.strip_suffix(']').map(str::to_string))
            .unwrap_or_else(|| text.trim().to_string()))
    }
}

/// Combined stats across every zram device.
pub fn total(root: &Root) -> Result<Stats> {
    let mut out = Stats::default();
    for d in Device::discover(root)? {
        let s = d.stats()?;
        out.orig_data_size += s.orig_data_size;
        out.compr_data_size += s.compr_data_size;
        out.mem_used_total += s.mem_used_total;
        out.mem_used_max += s.mem_used_max;
        out.same_pages += s.same_pages;
        out.huge_pages += s.huge_pages;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim from /sys/block/zram0 on the target machine.
    const MM_STAT: &str =
        "5206700032 1135610704 1166180352        0 1166180352     3858    17579    31616    32229\n";

    fn fixture(mm_stat: &str) -> (tempfile::TempDir, Root) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        let d = root.join("sys/block/zram0");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("mm_stat"), mm_stat).unwrap();
        fs::write(d.join("disksize"), "17179869184\n").unwrap();
        fs::write(
            d.join("comp_algorithm"),
            "lzo-rle lzo lz4 lz4hc [zstd] deflate 842 \n",
        )
        .unwrap();
        (dir, root)
    }

    #[test]
    fn parses_the_real_mm_stat() {
        let (_d, root) = fixture(MM_STAT);
        let dev = &Device::discover(&root).unwrap()[0];
        let s = dev.stats().unwrap();
        assert_eq!(s.orig_data_size, 5_206_700_032);
        assert_eq!(s.compr_data_size, 1_135_610_704);
        assert_eq!(s.mem_used_total, 1_166_180_352);
        assert_eq!(s.same_pages, 3_858);
        assert_eq!(s.huge_pages, 31_616);
    }

    /// The correction that stops v1's false alarm: ~4 GB of "used" memory is not
    /// really used.
    #[test]
    fn reports_how_much_ram_zram_is_saving() {
        let (_d, root) = fixture(MM_STAT);
        let s = total(&root).unwrap();
        assert_eq!(s.saved(), 5_206_700_032 - 1_166_180_352);
        assert!((s.ratio() - 4.465).abs() < 0.01, "{}", s.ratio());
    }

    #[test]
    fn an_older_kernel_with_fewer_columns_still_parses() {
        // 8 columns: no huge_pages_since.
        let (_d, root) = fixture("1000 500 600 0 600 10 20 30\n");
        let s = Device::discover(&root).unwrap()[0].stats().unwrap();
        assert_eq!(s.orig_data_size, 1000);
        assert_eq!(s.huge_pages, 30);
    }

    #[test]
    fn a_future_kernel_adding_columns_still_parses() {
        let (_d, root) = fixture("1000 500 600 0 600 10 20 30 40 50 60\n");
        let s = Device::discover(&root).unwrap()[0].stats().unwrap();
        assert_eq!(s.orig_data_size, 1000);
        assert_eq!(s.mem_used_total, 600);
    }

    #[test]
    fn a_truncated_mm_stat_is_an_error() {
        let (_d, root) = fixture("1000 500\n");
        assert!(Device::discover(&root).unwrap()[0].stats().is_err());
    }

    #[test]
    fn reads_the_bracketed_active_algorithm() {
        let (_d, root) = fixture(MM_STAT);
        assert_eq!(Device::discover(&root).unwrap()[0].algorithm().unwrap(), "zstd");
    }

    #[test]
    fn disksize_is_reported_but_is_not_a_memory_cost() {
        let (_d, root) = fixture(MM_STAT);
        let dev = &Device::discover(&root).unwrap()[0];
        assert_eq!(dev.disksize().unwrap(), 17_179_869_184);
        assert!(dev.stats().unwrap().mem_used_total < dev.disksize().unwrap());
    }

    #[test]
    fn a_machine_without_zram_reports_no_devices_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        assert!(Device::discover(&root).unwrap().is_empty());
        assert_eq!(total(&root).unwrap(), Stats::default());
        assert_eq!(total(&root).unwrap().ratio(), 1.0);
    }

    #[test]
    fn ratio_of_an_empty_device_does_not_divide_by_zero() {
        let (_d, root) = fixture("0 0 0 0 0 0 0 0 0\n");
        assert_eq!(Device::discover(&root).unwrap()[0].stats().unwrap().ratio(), 1.0);
    }
}
