/// What an action actually achieved.
///
/// Every actuator returns what *happened*, not what was attempted. The ladder
/// acts unattended, so a report of intentions would be indistinguishable from a
/// report of results to the person reading it later.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Processes or groups the action successfully touched.
    pub affected: Vec<i32>,
    /// Bytes of memory measurably returned, where measurable.
    pub bytes_freed: u64,
    /// Why the action did not happen, or did not fully happen. Empty on a clean
    /// success.
    pub notes: Vec<String>,
}

impl Outcome {
    pub fn refused(reason: impl Into<String>) -> Self {
        Outcome {
            notes: vec![reason.into()],
            ..Default::default()
        }
    }

    /// A success that touched these processes and measurably freed `bytes`.
    pub fn freed(affected: Vec<i32>, bytes: u64) -> Self {
        Outcome {
            affected,
            bytes_freed: bytes,
            notes: Vec::new(),
        }
    }

    pub fn did_nothing(&self) -> bool {
        self.affected.is_empty() && self.bytes_freed == 0
    }
}
