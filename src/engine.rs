use crate::Options;
use crate::client::ClientType;
use crate::client::hyper::*;
use crate::client::hyper_h2::*;
use crate::client::hyper_legacy::*;
use crate::client::hyper_multichunk::http_hyper_multichunk;
use crate::client::hyper_rt1::{RequestBody, http_hyper_rt1};
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use crate::client::io_uring::*;
use crate::client::reqwest::*;
use crate::client::utils::build_http_connection_legacy;
use crate::metrics::Metrics;
use crate::stats::RealtimeStats;
use crate::stats::Statistics;
use atomic_time::AtomicDuration;
use atomic_time::AtomicInstant;

use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use crossterm::{cursor, execute, terminal};
use tokio::runtime::Builder;
use tokio::task::JoinSet;

use anyhow::Result;
use std::sync::atomic::Ordering;

use tabled::builder::Builder as TableBuilder;
use tabled::settings::object::Columns;
use tabled::settings::{Alignment, Modify, Style, Width};

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

    let mut runtime_stats: Vec<RealtimeStats> = Vec::with_capacity(instances);
    runtime_stats.resize_with(instances, Default::default);
    let rt_stats = Arc::new(runtime_stats);

    let start = Instant::now();

    // spawn tasks...
    let meters = Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .build()
        .unwrap();

    let clone_stats = Arc::clone(&rt_stats);
    meters.spawn(async move {
        let ctrl_c_handle = tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            // Restore cursor on Ctrl+C
            let _ = execute!(std::io::stdout(), cursor::Show);
            println!();
            std::process::exit(0);
        });

        meter(clone_stats).await;

        ctrl_c_handle.abort();
    });

    let connections_per_instance = opts.connections / instances;
    let reminder = opts.connections % instances;

    for id in 0..instances {
        let mut opts = opts.clone();
        let stats = rt_stats.clone();
        opts.connections = if id < reminder {
            connections_per_instance + 1
        } else {
            connections_per_instance
        };

        let handle = thread::spawn(move || -> Result<(Statistics, Metrics)> {
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            if matches!(opts.client_type, ClientType::IoUring) {
                tokio_uring_thread(id, opts, stats)
            } else {
                tokio_thread(id, opts, stats)
            }
            #[cfg(not(all(target_os = "linux", feature = "io_uring")))]
            tokio_thread(id, opts, stats)
        });

        handles.push(handle);
    }

    let coll = handles.into_iter().map(|h| h.join().expect("thread error"));
    let out: Vec<(Statistics, Metrics)> = coll.collect::<Result<Vec<_>, _>>()?;
    let duration = start.elapsed().as_micros() as u64;

    let (total_stats, total_metrics) = out.into_iter().fold(
        (Statistics::default(), Metrics::default()),
        |(acc_s, mut acc_m), (s, m)| {
            acc_m.aggregate(&m);
            (acc_s + s, acc_m)
        },
    );

    let total = total_stats;
    print_results(&total, duration, opts.threads, opts.metrics, &total_metrics);
    Ok(())
}

fn pretty_lat(l: f64) -> String {
    if l >= 1_000_000.0 {
        format!("{:.2}s", l / 1_000_000.0)
    } else if l >= 1_000.0 {
        format!("{:.2}ms", l / 1_000.0)
    } else {
        format!("{:.2}µs", l)
    }
}

fn print_results(
    total: &Statistics,
    duration: u64,
    threads: usize,
    show_metrics: bool,
    total_metrics: &Metrics,
) {
    let total_ok = total.ok();
    let total_conn = total.conn();
    let total_3xx = total.status_3xx();
    let total_4xx = total.status_4xx();
    let total_5xx = total.status_5xx();
    let total_err = total.errors();
    let idle_perc = total.idle() / (threads as f64) * 100.0;

    // Summary table
    println!();
    println!("Summary:");

    let mut builder = TableBuilder::default();
    builder.push_record(["", "okay", "conn", "3xx", "4xx", "5xx", "err", "%idle"]);
    builder.push_record([
        "Total",
        &total_ok.to_string(),
        &total_conn.to_string(),
        &total_3xx.to_string(),
        &total_4xx.to_string(),
        &total_5xx.to_string(),
        &total_err.to_string(),
        &format!("{:.2}", idle_perc),
    ]);
    let table = builder
        .build()
        .with(Style::rounded())
        .with(Modify::new(Columns::new(1..)).with(Alignment::right()))
        .to_string();
    println!("{}", table);

    // Details table
    let ok_sec = if duration > 0 {
        total_ok * 1000000 / duration
    } else {
        0
    };
    let mut builder = TableBuilder::default();
    builder.push_record(["status", "total", "rate/sec"]);
    let mut has_details = false;

    if total_ok > 0 {
        builder.push_record(["200", &total_ok.to_string(), &ok_sec.to_string()]);
        has_details = true;
    }

    for (key, total_value) in total.http_status().iter() {
        let per_sec = if duration > 0 {
            total_value * 1000000 / duration
        } else {
            0
        };
        builder.push_record([
            &key.to_string(),
            &total_value.to_string(),
            &per_sec.to_string(),
        ]);
        has_details = true;
    }

    if has_details {
        println!();
        println!(" Details:");
        let table = builder
            .build()
            .with(Style::rounded())
            .with(Modify::new(Columns::new(1..)).with(Alignment::right()))
            .to_string();
        println!("{}", table);
    }

    // Errors table
    let errors: Vec<_> = total.errors_map().iter().collect();
    if !errors.is_empty() {
        println!();
        println!(" Errors:");
        let mut builder = TableBuilder::default();
        builder.push_record(["error", "count", "rate/sec"]);
        for (key, total_value) in &errors {
            let per_sec = if duration > 0 {
                *total_value * 1000000 / duration
            } else {
                0
            };
            let error_str = key.to_string();
            let truncated = if error_str.len() > 55 {
                format!("{}…", &error_str[..54])
            } else {
                error_str
            };
            builder.push_record([&truncated, &total_value.to_string(), &per_sec.to_string()]);
        }

        let table = builder
            .build()
            .with(Style::rounded())
            .with(Modify::new(Columns::first()).with(Width::truncate(55)))
            .with(Modify::new(Columns::new(1..)).with(Alignment::right()))
            .to_string();
        println!("{}", table);
    }

    // Latency table
    if let Some(ref latency) = total.latency {
        println!();
        println!(" Latency:");
        let mut builder = TableBuilder::default();
        builder.push_record(["", "p50", "p75", "p90", "p99"]);
        builder.push_record([
            "percentiles",
            &pretty_lat(latency.value_at_quantile(0.50) as f64),
            &pretty_lat(latency.value_at_quantile(0.75) as f64),
            &pretty_lat(latency.value_at_quantile(0.95) as f64),
            &pretty_lat(latency.value_at_quantile(0.99) as f64),
        ]);
        let table = builder
            .build()
            .with(Style::rounded())
            .with(Modify::new(Columns::new(1..)).with(Alignment::right()))
            .to_string();
        println!("{}", table);

        let mut builder = TableBuilder::default();
        builder.push_record(["", "min", "mean", "max"]);
        builder.push_record([
            "stats",
            &pretty_lat(latency.min() as f64),
            &pretty_lat(latency.mean()),
            &pretty_lat(latency.max() as f64),
        ]);
        let table = builder
            .build()
            .with(Style::rounded())
            .with(Modify::new(Columns::new(1..)).with(Alignment::right()))
            .to_string();
        println!("{}", table);
    }

    // Display metrics if enabled
    if show_metrics {
        total_metrics.display();
    }
}

fn tokio_thread(
    id: usize,
    opts: Options,
    rt_stats: Arc<Vec<RealtimeStats>>,
) -> Result<(Statistics, Metrics)> {
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
            .thread_name(format!("plumbrs-{}/s", id))
            .build()
            .unwrap(),

        Some(num_threads) => {
            #[cfg(tokio_unstable)]
            match opts.disable_lifo_slot {
                true => Builder::new_multi_thread()
                    .disable_lifo_slot()
                    .enable_all()
                    .worker_threads(num_threads)
                    .global_queue_interval(opts.global_queue_interval.unwrap_or(61))
                    .event_interval(opts.event_interval.unwrap_or(61))
                    .max_io_events_per_tick(opts.max_io_events_per_tick.unwrap_or(1024))
                    .thread_name(format!("plumbrs-{}/m", id))
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

                false => Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(num_threads)
                    .global_queue_interval(opts.global_queue_interval.unwrap_or(61))
                    .event_interval(opts.event_interval.unwrap_or(61))
                    .max_io_events_per_tick(opts.max_io_events_per_tick.unwrap_or(1024))
                    .thread_name(format!("plumbrs-{}/m", id))
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
                .thread_name(format!("plumbrs-{}/m", id))
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

    let mut stats = runtime.block_on(async { spawn_tasks(id, opts, rt_stats).await });

    stats.idle_time(
        total_park_time.load(Ordering::Relaxed).as_secs_f64() / start.elapsed().as_secs_f64(),
    );

    let metrics = Metrics::new(&runtime.metrics());

    Ok((stats, metrics))
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn tokio_uring_thread(
    id: usize,
    opts: Options,
    rt_stats: Arc<Vec<RealtimeStats>>,
) -> Result<(Statistics, Metrics)> {
    let metrics = Metrics::default();
    let opts = Arc::new(opts);

    let num_entries = opts.uring_entries.next_power_of_two();
    let cqsize = num_entries * 2;

    let mut uring = tokio_uring::uring_builder();

    uring.setup_single_issuer().setup_cqsize(cqsize);

    if let Some(idle) = opts.uring_sqpoll {
        uring.setup_sqpoll(idle);
    } else {
        uring.setup_coop_taskrun().setup_taskrun_flag();
    }

    let stats = tokio_uring::builder()
        .entries(num_entries) // Large ring size is critical for throughput
        .uring_builder(&uring)
        .start(async move {
            let handle = tokio_uring::spawn(async move { spawn_tasks(id, opts, rt_stats).await });

            handle.await.unwrap()
        });

    Ok((stats, metrics))
}

async fn spawn_tasks(
    id: usize,
    opts: Arc<Options>,
    rt_stats: Arc<Vec<RealtimeStats>>,
) -> Statistics {
    let mut tasks = JoinSet::new();
    let mut statistics = Statistics::default();

    let client = if matches!(opts.client_type, ClientType::HyperRt1) {
        Some(build_http_connection_legacy::<RequestBody>(&opts))
    } else {
        None
    };

    for con in 0..opts.connections {
        let opts = Arc::clone(&opts);
        let stats = Arc::clone(&rt_stats);

        match opts.client_type {
            ClientType::Auto => {
                if opts.body.len() > 1 {
                    tasks.spawn(
                        async move { http_hyper_multichunk(id, con, opts, &stats[id]).await },
                    );
                } else {
                    tasks.spawn(async move { http_hyper(id, con, opts, &stats[id]).await });
                }
            }
            ClientType::Hyper => {
                tasks.spawn(async move { http_hyper(id, con, opts, &stats[id]).await });
            }
            ClientType::HyperMultichunk => {
                tasks.spawn(async move { http_hyper_multichunk(id, con, opts, &stats[id]).await });
            }
            ClientType::HyperLegacy => {
                tasks.spawn(async move { http_hyper_legacy(id, con, opts, &stats[id]).await });
            }
            ClientType::HyperRt1 => {
                let con_client = client.as_ref().unwrap().clone();
                tasks.spawn(
                    async move { http_hyper_rt1(id, con, opts, con_client, &stats[id]).await },
                );
            }
            ClientType::HyperH2 => {
                tasks.spawn(async move { http_hyper_h2(id, con, opts, &stats[id]).await });
            }
            ClientType::Reqwest => {
                tasks.spawn(async move { http_reqwest(id, con, opts, &stats[id]).await });
            }
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            ClientType::IoUring => {
                tasks.spawn_local(async move { http_io_uring(id, con, opts, &stats[id]).await });
            }
            ClientType::Help => (),
        }
    }

    while let Some(res) = tasks.join_next().await {
        match res {
            Ok(s) => statistics = statistics + s,
            Err(err) => {
                if opts.verbose {
                    eprintln!("Unable to join task: {}", err);
                }
            }
        }
    }

    statistics
}

pub async fn meter(rt_stats: Arc<Vec<RealtimeStats>>) {
    const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut spinner_idx = 0;

    loop {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let mut total_ok = 0u64;
        let mut total_err = 0u64;
        let mut total_fail = 0u64;

        for stats in rt_stats.iter() {
            total_ok += stats.ok.swap(0, Ordering::Relaxed);
            total_err += stats.err.swap(0, Ordering::Relaxed);
            total_fail += stats.fail.swap(0, Ordering::Relaxed);
        }

        print!(
            "\r{} Stats: ok: {total_ok}/sec, fail: {total_fail}/sec, err: {total_err}/sec",
            SPINNER[spinner_idx]
        );
        let _ = execute!(
            std::io::stdout(),
            terminal::Clear(terminal::ClearType::UntilNewLine)
        );

        spinner_idx = (spinner_idx + 1) % SPINNER.len();
    }
}
