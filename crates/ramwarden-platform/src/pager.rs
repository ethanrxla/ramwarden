use crate::Outcome;

/// Pushes one process's cold memory out of RAM.
///
/// Finer than group reclaim: on Linux it targets individual cold mappings, on
/// Windows it trims one process's working set. Optional on both — without it
/// the ladder leans on group reclaim, which is a normal configuration.
pub trait Pager: Send + Sync {
    fn available(&self) -> bool;

    /// Where page-out would come from, for the status line.
    fn route(&self) -> &'static str;

    fn page_out(&self, pid: i32, referenced_ratio: f64, min_bytes: u64) -> Outcome;
}

/// The pager on a platform with no page-out route at all.
pub struct NoPager;

impl Pager for NoPager {
    fn available(&self) -> bool {
        false
    }

    fn route(&self) -> &'static str {
        "unavailable — group reclaim only"
    }

    fn page_out(&self, pid: i32, _: f64, _: u64) -> Outcome {
        Outcome::refused(format!("pid {pid}: no page-out route on this platform"))
    }
}
