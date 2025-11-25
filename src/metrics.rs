#[cfg(target_has_atomic = "64")]
use std::time::Duration;
use tokio::runtime::RuntimeMetrics as TokioRuntimeMetrics;

#[derive(Debug, Clone, Default)]
pub struct Metrics {
    num_workers: usize,
    num_alive_tasks: usize,
    global_queue_depth: usize,
    #[cfg(target_has_atomic = "64")]
    worker_total_busy_duration: Duration,
    #[cfg(target_has_atomic = "64")]
    worker_park_count: u64,
    #[cfg(target_has_atomic = "64")]
    worker_park_unpark_count: u64,
    #[cfg(tokio_unstable)]
    worker_local_queue_depth: usize,
    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    spawned_tasks_count: u64,
    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    budget_forced_yield_count: u64,
    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    worker_noop_count: u64,
    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    worker_steal_count: u64,
    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    worker_steal_operations: u64,
    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    worker_poll_count: u64,
    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    worker_local_schedule_count: u64,
    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    worker_overflow_count: u64,
    #[cfg(tokio_unstable)]
    worker_mean_poll_time: Duration,
    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    io_driver_fd_registered_count: u64,
    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    io_driver_fd_deregistered_count: u64,
    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    io_driver_ready_count: u64,
}

impl Metrics {
    /// Create a new Metrics struct from Tokio's RuntimeMetrics
    pub fn new(tokio_metrics: &TokioRuntimeMetrics) -> Self {
        let num_workers = tokio_metrics.num_workers();

        let mut metrics = Metrics {
            num_workers,
            num_alive_tasks: tokio_metrics.num_alive_tasks(),
            global_queue_depth: tokio_metrics.global_queue_depth(),
            #[cfg(target_has_atomic = "64")]
            worker_total_busy_duration: Duration::default(),
            #[cfg(target_has_atomic = "64")]
            worker_park_count: 0,
            #[cfg(target_has_atomic = "64")]
            worker_park_unpark_count: 0,
            #[cfg(tokio_unstable)]
            worker_local_queue_depth: 0,
            #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
            spawned_tasks_count: 0,
            #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
            budget_forced_yield_count: 0,
            #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
            worker_noop_count: 0,
            #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
            worker_steal_count: 0,
            #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
            worker_steal_operations: 0,
            #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
            worker_poll_count: 0,
            #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
            worker_local_schedule_count: 0,
            #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
            worker_overflow_count: 0,
            #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
            worker_mean_poll_time: Duration::default(),
            #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
            io_driver_fd_registered_count: 0,
            #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
            io_driver_fd_deregistered_count: 0,
            #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
            io_driver_ready_count: 0,
        };

        // Aggregate worker-specific metrics across all workers
        #[cfg(target_has_atomic = "64")]
        {
            for worker_id in 0..num_workers {
                metrics.worker_total_busy_duration += tokio_metrics.worker_total_busy_duration(worker_id);
                metrics.worker_park_count += tokio_metrics.worker_park_count(worker_id);
                metrics.worker_park_unpark_count += tokio_metrics.worker_park_unpark_count(worker_id);
            }
        }

        // Aggregate target_has_atomic metrics that require tokio_unstable API
        #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
        {
            for worker_id in 0..num_workers {
                metrics.worker_noop_count += tokio_metrics.worker_noop_count(worker_id);
                metrics.worker_steal_count += tokio_metrics.worker_steal_count(worker_id);
                metrics.worker_steal_operations += tokio_metrics.worker_steal_operations(worker_id);
                metrics.worker_poll_count += tokio_metrics.worker_poll_count(worker_id);
                metrics.worker_local_schedule_count += tokio_metrics.worker_local_schedule_count(worker_id);
                metrics.worker_overflow_count += tokio_metrics.worker_overflow_count(worker_id);
            }

            // Global metrics (not per-worker)
            metrics.spawned_tasks_count = tokio_metrics.spawned_tasks_count();
            metrics.budget_forced_yield_count = tokio_metrics.budget_forced_yield_count();
            metrics.io_driver_fd_registered_count = tokio_metrics.io_driver_fd_registered_count();
            metrics.io_driver_fd_deregistered_count = tokio_metrics.io_driver_fd_deregistered_count();
            metrics.io_driver_ready_count = tokio_metrics.io_driver_ready_count();
        }

        // Aggregate tokio_unstable metrics
        #[cfg(tokio_unstable)]
        {
            for worker_id in 0..num_workers {
                metrics.worker_local_queue_depth += tokio_metrics.worker_local_queue_depth(worker_id);
                metrics.worker_mean_poll_time += tokio_metrics.worker_mean_poll_time(worker_id);
            }

            // Average the mean poll time across workers
            if num_workers > 0 {
                metrics.worker_mean_poll_time /= num_workers as u32;
            }
        }

        metrics
    }

    /// Aggregate another Metrics instance into this one
    pub fn aggregate(&mut self, other: &Metrics) {
        self.num_workers += other.num_workers;
        self.num_alive_tasks += other.num_alive_tasks;
        self.global_queue_depth += other.global_queue_depth;

        #[cfg(target_has_atomic = "64")]
        {
            self.worker_total_busy_duration += other.worker_total_busy_duration;
            self.worker_park_count += other.worker_park_count;
            self.worker_park_unpark_count += other.worker_park_unpark_count;
        }

        #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
        {
            self.spawned_tasks_count += other.spawned_tasks_count;
            self.budget_forced_yield_count += other.budget_forced_yield_count;
            self.worker_noop_count += other.worker_noop_count;
            self.worker_steal_count += other.worker_steal_count;
            self.worker_steal_operations += other.worker_steal_operations;
            self.worker_poll_count += other.worker_poll_count;
            self.worker_local_schedule_count += other.worker_local_schedule_count;
            self.worker_overflow_count += other.worker_overflow_count;
            self.io_driver_fd_registered_count += other.io_driver_fd_registered_count;
            self.io_driver_fd_deregistered_count += other.io_driver_fd_deregistered_count;
            self.io_driver_ready_count += other.io_driver_ready_count;
        }

        #[cfg(tokio_unstable)]
        {
            // Use exponentially weighted moving average (EWMA) for queue depth and mean poll time
            // New value = old_value * 0.8 + new_value * 0.2
            self.worker_local_queue_depth =
                (self.worker_local_queue_depth * 4 + other.worker_local_queue_depth) / 5;

            self.worker_mean_poll_time =
                (self.worker_mean_poll_time * 4 + other.worker_mean_poll_time) / 5;
        }
    }

    // Getter methods for accessing metrics
    pub fn num_workers(&self) -> usize {
        self.num_workers
    }

    pub fn num_alive_tasks(&self) -> usize {
        self.num_alive_tasks
    }

    pub fn global_queue_depth(&self) -> usize {
        self.global_queue_depth
    }

    #[cfg(target_has_atomic = "64")]
    pub fn worker_total_busy_duration(&self) -> Duration {
        self.worker_total_busy_duration
    }

    #[cfg(target_has_atomic = "64")]
    pub fn worker_park_count(&self) -> u64 {
        self.worker_park_count
    }

    #[cfg(target_has_atomic = "64")]
    pub fn worker_park_unpark_count(&self) -> u64 {
        self.worker_park_unpark_count
    }

    #[cfg(tokio_unstable)]
    pub fn worker_local_queue_depth(&self) -> usize {
        self.worker_local_queue_depth
    }

    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    pub fn spawned_tasks_count(&self) -> u64 {
        self.spawned_tasks_count
    }

    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    pub fn budget_forced_yield_count(&self) -> u64 {
        self.budget_forced_yield_count
    }

    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    pub fn worker_noop_count(&self) -> u64 {
        self.worker_noop_count
    }

    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    pub fn worker_steal_count(&self) -> u64 {
        self.worker_steal_count
    }

    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    pub fn worker_steal_operations(&self) -> u64 {
        self.worker_steal_operations
    }

    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    pub fn worker_poll_count(&self) -> u64 {
        self.worker_poll_count
    }

    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    pub fn worker_local_schedule_count(&self) -> u64 {
        self.worker_local_schedule_count
    }

    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    pub fn worker_overflow_count(&self) -> u64 {
        self.worker_overflow_count
    }

    #[cfg(tokio_unstable)]
    pub fn worker_mean_poll_time(&self) -> Duration {
        self.worker_mean_poll_time
    }

    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    pub fn io_driver_fd_registered_count(&self) -> u64 {
        self.io_driver_fd_registered_count
    }

    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    pub fn io_driver_fd_deregistered_count(&self) -> u64 {
        self.io_driver_fd_deregistered_count
    }

    #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
    pub fn io_driver_ready_count(&self) -> u64 {
        self.io_driver_ready_count
    }

    /// Display metrics in a human-readable format
    pub fn display(&self) {
        println!(" Runtime Configuration:");
        println!("   Workers:              {:>10}", self.num_workers);
        println!("   Alive Tasks:          {:>10}", self.num_alive_tasks);
        println!("   Global Queue Depth:   {:>10}", self.global_queue_depth);
        println!("─────────────────────────────────────────────────────────────────────");

        #[cfg(target_has_atomic = "64")]
        {
            println!(" Worker Metrics:");
            println!("   Total Busy Duration:  {:>10.3}s", self.worker_total_busy_duration.as_secs_f64());
            println!("   Park Count:           {:>10}", self.worker_park_count);
            println!("   Park/Unpark Count:    {:>10}", self.worker_park_unpark_count);
            println!("─────────────────────────────────────────────────────────────────────");
        }

        #[cfg(all(tokio_unstable, target_has_atomic = "64"))]
        {
            println!(" Task Metrics:");
            println!("   Spawned Tasks:        {:>10}", self.spawned_tasks_count);
            println!("   Budget Forced Yields: {:>10}", self.budget_forced_yield_count);
            println!("─────────────────────────────────────────────────────────────────────");

            println!(" Scheduling Metrics:");
            println!("   No-op Count:          {:>10}", self.worker_noop_count);
            println!("   Poll Count:           {:>10}", self.worker_poll_count);
            println!("   Local Schedule Count: {:>10}", self.worker_local_schedule_count);
            println!("   Overflow Count:       {:>10}", self.worker_overflow_count);
            println!("─────────────────────────────────────────────────────────────────────");

            println!(" Work Stealing Metrics:");
            println!("   Steal Count:          {:>10}", self.worker_steal_count);
            println!("   Steal Operations:     {:>10}", self.worker_steal_operations);
            if self.worker_steal_operations > 0 {
                let avg_steals = self.worker_steal_count as f64 / self.worker_steal_operations as f64;
                println!("   Avg Tasks/Steal:      {:>10.2}", avg_steals);
            }
            println!("─────────────────────────────────────────────────────────────────────");

            println!(" I/O Driver Metrics:");
            println!("   FDs Registered:       {:>10}", self.io_driver_fd_registered_count);
            println!("   FDs Deregistered:     {:>10}", self.io_driver_fd_deregistered_count);
            let active_fds = self.io_driver_fd_registered_count.saturating_sub(self.io_driver_fd_deregistered_count);
            println!("   Active FDs:           {:>10}", active_fds);
            println!("   Ready Events:         {:>10}", self.io_driver_ready_count);
            println!("─────────────────────────────────────────────────────────────────────");
        }

        #[cfg(tokio_unstable)]
        {
            println!(" Worker Queue Metrics:");
            println!("   Local Queue Depth:    {:>10}", self.worker_local_queue_depth);
            println!("   Mean Poll Time:       {:>10.3}µs", self.worker_mean_poll_time.as_micros());
            println!("─────────────────────────────────────────────────────────────────────");
        }
    }
}
