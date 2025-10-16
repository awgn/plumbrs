use crate::Options;
use atomic_time::AtomicDuration;
use atomic_time::AtomicInstant;
use crate::client::ClientType;
use crate::client::hyper::*;
use crate::client::hyper_1rt::http_hyper_1rt;
use crate::client::hyper_h2::*;
use crate::client::hyper_legacy::*;
use crate::client::reqwest::*;
use crate::client::utils::build_http_connection_legacy;
use crate::stats::Statistics;

use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use tokio::runtime::Builder;
use tokio::task::JoinSet;

pub mod metrics;
use anyhow::Result;
use metrics::AggRuntimeMetrics;
use std::sync::atomic::Ordering;

pub fn run_tokio_engines(opts: Options) -> Result<()> {
    let mut handles: Vec<_> = Vec::with_capacity(opts.threads);
    let instances = opts.threads / opts.multithreaded.unwrap_or(1);

    println!(
        "{} {} tokio runtime{} started ({} total connections, {} per thread)",
        instances,
        match opts.multithreaded {
            None => "single-threaded".to_string(),
            Some(n) => format!("multi-threaded/{}", n),
        },
        if opts.threads > 1 { "s" } else { "" },
        opts.connections,
        opts.connections / opts.threads
    );

    let start = Instant::now();
    for i in 0..instances {
        let mut opts = opts.clone();
        opts.connections /= instances; // number of connection per engine
        let handle = thread::spawn(move || -> Result<(Statistics, Option<AggRuntimeMetrics>)> {
            runtime_engine(i, opts)
        });

        handles.push(handle);
    }

    let coll = handles.into_iter().map(|h| h.join().expect("thread error"));
    let out = coll.collect::<Result<Vec<_>, _>>()?;
    let duration = start.elapsed().as_micros() as u64;

    let (total, metrics) = out.into_iter().fold(
        (Statistics::default(), Some(AggRuntimeMetrics::default())),
        |(acc_s, acc_m), (s, m)| {
            let m = match (acc_m, m) {
                (Some(acc_m), Some(m)) => Some(acc_m + m),
                _ => None,
            };
            (acc_s + s, m)
        },
    );

    println!(
        "Stats: ok:{} err:{}, 3xx:{}, 4xx:{}, 5xx:{}, idle:{:.2}%",
        total.ok,
        total.total_errors(),
        total.total_status_3xx(),
        total.total_status_4xx(),
        total.total_status_5xx(),
        total.idle / (opts.threads as f64) * 100.0
    );

    println!("OK: {}/sec", total.ok * 1000000 / duration);
    for (key, total_value) in total.http_status {
        println!("HTTP {}: {}/sec", key, total_value * 1000000 / duration);
    }

    for (key, total_value) in total.err {
        println!("{:?}: {}/sec", key, total_value * 1000000 / duration);
    }

    if let Some(metrics) = metrics {
        println!("Tokio Metrics: {:#?}", metrics);
    }

    Ok(())
}

fn runtime_engine(id: usize, opts: Options) -> Result<(Statistics, Option<AggRuntimeMetrics>)> {
    let opts = Arc::new(opts);
    let start = Instant::now();
    let park_time = Arc::new(AtomicInstant::new(start));
    let total_park_time = Arc::new(AtomicDuration::new(Duration::default()));

    let runtime = match opts.multithreaded {
        None => Builder::new_current_thread()
            .enable_all()
            .worker_threads(1)
            .global_queue_interval(opts.global_queue_interval.unwrap_or(31))
            .event_interval(opts.event_interval.unwrap_or(61))
            .max_io_events_per_tick(opts.max_io_events_per_tick.unwrap_or(1024))
            .thread_name(format!("plumbr-{}/s", id))
            .build()
            .unwrap(),

        Some(num_threads) => {
            #[cfg(tokio_unstable)]
            match (opts.multithread_alt, opts.disable_lifo_slot) {
                (true, true) => Builder::new_multi_thread_alt()
                    .disable_lifo_slot()
                    .enable_all()
                    .worker_threads(num_threads)
                    .global_queue_interval(opts.global_queue_interval.unwrap_or(61))
                    .event_interval(opts.event_interval.unwrap_or(61))
                    .max_io_events_per_tick(opts.max_io_events_per_tick.unwrap_or(1024))
                    .thread_name(format!("plumbr-{}/m", id))
                    .on_thread_park({
                        let park_time = Arc::clone(&park_time);
                        move || {
                            park_time.store(Instant::now(), Ordering::Relaxed);
                        }
                    })
                    .on_thread_unpark({
                        let park_time = Arc::clone(&park_time);
                        let total_park_time = Arc::clone(&total_park_time);
                        move || {
                            let delta = Instant::now() - park_time.load(Ordering::Relaxed);
                            total_park_time.store(
                                total_park_time.load(Ordering::Relaxed) + delta,
                                Ordering::Relaxed,
                            );
                        }
                    })
                    .build()
                    .unwrap(),

                (true, false) => Builder::new_multi_thread_alt()
                    .enable_all()
                    .worker_threads(num_threads)
                    .global_queue_interval(opts.global_queue_interval.unwrap_or(61))
                    .event_interval(opts.event_interval.unwrap_or(61))
                    .max_io_events_per_tick(opts.max_io_events_per_tick.unwrap_or(1024))
                    .thread_name(format!("plumbr-{}/m", id))
                    .on_thread_park({
                        let park_time = Arc::clone(&park_time);
                        move || {
                            park_time.store(Instant::now(), Ordering::Relaxed);
                        }
                    })
                    .on_thread_unpark({
                        let park_time = Arc::clone(&park_time);
                        let total_park_time = Arc::clone(&total_park_time);
                        move || {
                            let delta = Instant::now() - park_time.load(Ordering::Relaxed);
                            total_park_time.store(
                                total_park_time.load(Ordering::Relaxed) + delta,
                                Ordering::Relaxed,
                            );
                        }
                    })
                    .build()
                    .unwrap(),

                (false, true) => Builder::new_multi_thread()
                    .disable_lifo_slot()
                    .enable_all()
                    .worker_threads(num_threads)
                    .global_queue_interval(opts.global_queue_interval.unwrap_or(61))
                    .event_interval(opts.event_interval.unwrap_or(61))
                    .max_io_events_per_tick(opts.max_io_events_per_tick.unwrap_or(1024))
                    .thread_name(format!("plumbr-{}/m", id))
                    .on_thread_park({
                        let park_time = Arc::clone(&park_time);
                        move || {
                            park_time.store(Instant::now(), Ordering::Relaxed);
                        }
                    })
                    .on_thread_unpark({
                        let park_time = Arc::clone(&park_time);
                        let total_park_time = Arc::clone(&total_park_time);
                        move || {
                            let delta = Instant::now() - park_time.load(Ordering::Relaxed);
                            total_park_time.store(
                                total_park_time.load(Ordering::Relaxed) + delta,
                                Ordering::Relaxed,
                            );
                        }
                    })
                    .build()
                    .unwrap(),

                (false, false) => Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(num_threads)
                    .global_queue_interval(opts.global_queue_interval.unwrap_or(61))
                    .event_interval(opts.event_interval.unwrap_or(61))
                    .max_io_events_per_tick(opts.max_io_events_per_tick.unwrap_or(1024))
                    .thread_name(format!("plumbr-{}/m", id))
                    .on_thread_park({
                        let park_time = Arc::clone(&park_time);
                        move || {
                            park_time.store(Instant::now(), Ordering::Relaxed);
                        }
                    })
                    .on_thread_unpark({
                        let park_time = Arc::clone(&park_time);
                        let total_park_time = Arc::clone(&total_park_time);
                        move || {
                            let delta = Instant::now() - park_time.load(Ordering::Relaxed);
                            total_park_time.store(
                                total_park_time.load(Ordering::Relaxed) + delta,
                                Ordering::Relaxed,
                            );
                        }
                    })
                    .build()
                    .unwrap(),
            }

            #[cfg(not(tokio_unstable))]
            Builder::new_multi_thread()
                .enable_all()
                .worker_threads(num_threads)
                .global_queue_interval(opts.global_queue_interval.unwrap_or(61))
                .event_interval(opts.event_interval.unwrap_or(61))
                .max_io_events_per_tick(opts.max_io_events_per_tick.unwrap_or(1024))
                .thread_name(format!("plumbr-{}/m", id))
                .on_thread_park({
                    let park_time = Arc::clone(&park_time);
                    move || {
                        park_time.store(Instant::now(), Ordering::Relaxed);
                    }
                })
                .on_thread_unpark({
                    let park_time = Arc::clone(&park_time);
                    let total_park_time = Arc::clone(&total_park_time);
                    move || {
                        let delta = Instant::now() - park_time.load(Ordering::Relaxed);
                        total_park_time.store(
                            total_park_time.load(Ordering::Relaxed) + delta,
                            Ordering::Relaxed,
                        );
                    }
                })
                .build()
                .unwrap()
        }
    };

    // spawn tasks...

    let (mut stats, metrics) = runtime.block_on(async { spawn_tasks(id, opts).await });

    stats.idle =
        total_park_time.load(Ordering::Relaxed).as_secs_f64() / start.elapsed().as_secs_f64();
    Ok((stats, metrics))
}

async fn spawn_tasks(
    id: usize,
    opts: Arc<Options>,
) -> (Statistics, Option<metrics::AggRuntimeMetrics>) {
    #[cfg(all(tokio_unstable, feature = "tokio_metrics"))]
    let mut metrics_iter = {
        let handle = tokio::runtime::Handle::current();
        let runtime_monitor = tokio_metrics::RuntimeMonitor::new(&handle);
        runtime_monitor.intervals()
    };

    #[cfg(not(feature = "tokio_metrics"))]
    let mut metrics_iter = metrics::DummyMetricsIter;

    let mut tasks = JoinSet::new();
    let mut stats = Statistics::default();

    let client = if matches!(opts.client_type, ClientType::Hyper1Rt) {
        Some(build_http_connection_legacy(&opts))
    } else {
        None
    };

    for con in 0..opts.connections {
        let opts = Arc::clone(&opts);

        match opts.client_type {
            ClientType::Hyper => {
                tasks.spawn(async move { http_hyper(id, con, opts).await });
            }
            ClientType::HyperLegacy => {
                tasks.spawn(async move { http_hyper_legacy(id, con, opts).await });
            }
            ClientType::Hyper1Rt => {
                let con_client = client.as_ref().unwrap().clone();
                tasks.spawn(async move { http_hyper_1rt(id, con, opts, con_client).await });
            }
            ClientType::HyperH2 => {
                tasks.spawn(async move { http_hyper_h2(id, con, opts).await });
            }
            ClientType::Reqwest => {
                tasks.spawn(async move { http_reqwest(id, con, opts).await });
            }
            ClientType::Help => (),
        }
    }

    let mut tmp = stats.clone();

    while let Some(res) = tasks.join_next().await {
        match res {
            Ok(stats) => tmp = tmp + stats,
            Err(err) => {
                if opts.verbose {
                    eprintln!("Unable to join task: {}", err);
                }
            }
        }
    }

    stats = tmp;

    (stats, metrics_iter.next())
}
