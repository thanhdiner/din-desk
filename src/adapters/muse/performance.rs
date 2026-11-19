//! Per-process switches allow the same executable/workload to be compared.
use std::time::Duration;

#[derive(Clone, Copy)]
pub(super) struct Performance {
    pub warm_connections: usize,
    pub probe_after: Duration,
}
impl Performance {
    pub fn from_env() -> Self {
        let baseline = crate::config::performance_baseline();
        Self {
            warm_connections: if baseline { 0 } else { 2 },
            probe_after: if baseline {
                Duration::ZERO
            } else {
                Duration::from_secs(20)
            },
        }
    }
}
