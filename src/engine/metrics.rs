use std::cmp::{max, min};
use std::time::Duration;

/// Aggregate tokio metrics from tokio-metrics.
/// Some values are ignored, because we cannot aggregate
/// from multiple runtimes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AggRuntimeMetrics {
    pub workers_count: usize,
    pub total_park_count: u64,
    pub max_park_count: u64,
    pub min_park_count: u64,
    //pub mean_poll_duration: Duration,
    //pub mean_poll_duration_worker_min: Duration,
    //pub mean_poll_duration_worker_max: Duration,
    //pub poll_count_histogram: Vec<u64>,
    pub total_noop_count: u64,
    pub max_noop_count: u64,
    pub min_noop_count: u64,
    pub total_steal_count: u64,
    pub max_steal_count: u64,
    pub min_steal_count: u64,
    pub total_steal_operations: u64,
    pub max_steal_operations: u64,
    pub min_steal_operations: u64,
    pub num_remote_schedules: u64,
    pub total_local_schedule_count: u64,
    pub max_local_schedule_count: u64,
    pub min_local_schedule_count: u64,
    pub total_overflow_count: u64,
    pub max_overflow_count: u64,
    pub min_overflow_count: u64,
    pub total_polls_count: u64,
    pub max_polls_count: u64,
    pub min_polls_count: u64,
    pub total_busy_duration: Duration,
    pub max_busy_duration: Duration,
    pub min_busy_duration: Duration,
    pub injection_queue_depth: usize,
    pub total_local_queue_depth: usize,
    pub max_local_queue_depth: usize,
    pub min_local_queue_depth: usize,
    //pub elapsed: Duration,
    pub budget_forced_yield_count: u64,
    pub io_driver_ready_count: u64,
}

impl std::ops::Add for AggRuntimeMetrics {
    type Output = Self;
    fn add(mut self, other: AggRuntimeMetrics) -> Self {
        self.workers_count += other.workers_count;
        self.total_park_count += other.total_park_count;
        self.max_park_count = max(self.max_park_count, other.max_park_count);
        self.min_park_count = min(self.min_park_count, other.min_park_count);
        //self.mean_poll_duration += other.mean_poll_duration;
        //self.mean_poll_duration_worker_min += other.mean_poll_duration_worker_min;
        //self.mean_poll_duration_worker_max += other.mean_poll_duration_worker_max;
        //self.poll_count_histogram += other.poll_count_histogram;
        self.total_noop_count += other.total_noop_count;
        self.max_noop_count = max(self.max_noop_count, other.max_noop_count);
        self.min_noop_count = min(self.min_noop_count, other.min_noop_count);
        self.total_steal_count += other.total_steal_count;
        self.max_steal_count = max(self.max_steal_count, other.max_steal_count);
        self.min_steal_count = min(self.min_steal_count, other.min_steal_count);
        self.total_steal_operations += other.total_steal_operations;
        self.max_steal_operations = max(self.max_steal_operations, other.max_steal_operations);
        self.min_steal_operations = min(self.min_steal_operations, other.min_steal_operations);
        self.num_remote_schedules += other.num_remote_schedules;
        self.total_local_schedule_count += other.total_local_schedule_count;
        self.max_local_schedule_count = max(
            self.max_local_schedule_count,
            other.max_local_schedule_count,
        );
        self.min_local_schedule_count = min(
            self.min_local_schedule_count,
            other.min_local_schedule_count,
        );
        self.total_overflow_count += other.total_overflow_count;
        self.max_overflow_count = max(self.max_overflow_count, other.max_overflow_count);
        self.min_overflow_count = min(self.min_overflow_count, other.min_overflow_count);
        self.total_polls_count += other.total_polls_count;
        self.max_polls_count = max(self.max_polls_count, other.max_polls_count);
        self.min_polls_count = min(self.min_polls_count, other.min_polls_count);
        self.total_busy_duration += other.total_busy_duration;
        self.max_busy_duration = max(self.max_busy_duration, other.max_busy_duration);
        self.min_busy_duration = min(self.min_busy_duration, other.min_busy_duration);
        self.injection_queue_depth += other.injection_queue_depth;
        self.total_local_queue_depth += other.total_local_queue_depth;
        self.max_local_queue_depth = max(self.max_local_queue_depth, other.max_local_queue_depth);
        self.min_local_queue_depth += other.min_local_queue_depth;
        //self.elapsed += other.elapsed;
        self.budget_forced_yield_count += other.budget_forced_yield_count;
        self.io_driver_ready_count += other.io_driver_ready_count;
        self
    }
}

/// A fake metrics iterator
#[derive(Debug)]
pub struct DummyMetricsIter;

impl Iterator for DummyMetricsIter {
    type Item = AggRuntimeMetrics;
    fn next(&mut self) -> Option<Self::Item> {
        None
    }
}

#[cfg(all(tokio_unstable, feature = "tokio_metrics"))]
mod tokio {
    use super::AggRuntimeMetrics;
    use tokio_metrics::RuntimeMetrics;

    impl From<Option<RuntimeMetrics>> for AggRuntimeMetrics {
        fn from(m: Option<RuntimeMetrics>) -> Self {
            match m {
                None => Self::from(RuntimeMetrics::default()),
                Some(m) => Self::from(m),
            }
        }
    }

    impl From<RuntimeMetrics> for AggRuntimeMetrics {
        fn from(m: RuntimeMetrics) -> Self {
            AggRuntimeMetrics {
                workers_count: m.workers_count,
                total_park_count: m.total_park_count,
                max_park_count: m.max_park_count,
                min_park_count: m.min_park_count,
                //mean_poll_duration: m.mean_poll_duration,
                //mean_poll_duration_worker_min: m.mean_poll_duration_worker_min,
                //mean_poll_duration_worker_max: m.mean_poll_duration_worker_max,
                //poll_count_histogram: m.poll_count_histogram,
                total_noop_count: m.total_noop_count,
                max_noop_count: m.max_noop_count,
                min_noop_count: m.min_noop_count,
                total_steal_count: m.total_steal_count,
                max_steal_count: m.max_steal_count,
                min_steal_count: m.min_steal_count,
                total_steal_operations: m.total_steal_operations,
                max_steal_operations: m.max_steal_operations,
                min_steal_operations: m.min_steal_operations,
                num_remote_schedules: m.num_remote_schedules,
                total_local_schedule_count: m.total_local_schedule_count,
                max_local_schedule_count: m.max_local_schedule_count,
                min_local_schedule_count: m.min_local_schedule_count,
                total_overflow_count: m.total_overflow_count,
                max_overflow_count: m.max_overflow_count,
                min_overflow_count: m.min_overflow_count,
                total_polls_count: m.total_polls_count,
                max_polls_count: m.max_polls_count,
                min_polls_count: m.min_polls_count,
                total_busy_duration: m.total_busy_duration,
                max_busy_duration: m.max_busy_duration,
                min_busy_duration: m.min_busy_duration,
                injection_queue_depth: m.injection_queue_depth,
                total_local_queue_depth: m.total_local_queue_depth,
                max_local_queue_depth: m.max_local_queue_depth,
                min_local_queue_depth: m.min_local_queue_depth,
                //elapsed: m.elapsed,
                budget_forced_yield_count: m.budget_forced_yield_count,
                io_driver_ready_count: m.io_driver_ready_count,
            }
        }
    }
}
