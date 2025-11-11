use crate::Options;
use atomic_time::AtomicDuration;
use atomic_time::AtomicInstant;
use crate::client::ClientType;
use crate::client::hyper::*;
use crate::client::hyper_1rt::{http_hyper_1rt, RequestBody};
use crate::client::hyper_h2::*;
use crate::client::hyper_legacy::*;
use crate::client::reqwest::*;
use crate::client::utils::build_http_connection_legacy;
use crate::stats::RealtimeStats;
use crate::stats::Statistics;

use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use scopeguard::defer;
use tokio::runtime::Builder;
use tokio::task::JoinSet;
use crossterm::{cursor, execute, terminal};

use anyhow::Result;
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

    for id in 0..instances {
        let mut opts = opts.clone();
        let stats = rt_stats.clone();
        opts.connections /= instances; // number of connection per engine
        let handle = thread::spawn(move || -> Result<Statistics> {
            runtime_engine(id, opts, stats)
        });

        handles.push(handle);
    }

    let coll = handles.into_iter().map(|h| h.join().expect("thread error"));
    let out = coll.collect::<Result<Vec<_>, _>>()?;
    let duration = start.elapsed().as_micros() as u64;

    let total = out.into_iter().fold(
        Statistics::default(),
        |acc_s,s| {
            acc_s + s
        },
    );

    println!("\n─────────────────────────────────────────────────────────────────");

    let total_ok = total.total_ok();
    let total_3xx = total.total_status_3xx();
    let total_4xx = total.total_status_4xx();
    let total_5xx = total.total_status_5xx();
    let total_err = total.total_errors();
    let idle_perc = total.total_idle() / (opts.threads as f64) * 100.0;

    // Total statistics
    println!(" Total:   okay:{:<10}  3xx:{:<10}  4xx:{:<10}  5xx:{:<10} ",
        total_ok,
        total_3xx,
        total_4xx,
        total_5xx,
    );

    // HTTP status codes summary
    println!("          err:{:<10}   %idle:{:<10} ",
        total_err,
        idle_perc
    );

    // Throughput
    let ok_sec = total.total_ok() * 1000000 / duration;

    // HTTP status codes details
    println!("─────────────────────────────────────────────────────────────────");
    println!(" Details:                                                        ");
    if total_ok > 0 {
        println!("   200:   total:{:<10} rate/sec:{:<10}", total_ok, ok_sec);
    }

    let http_statuses: Vec<_> = total.get_http_status().iter().collect();
    if !http_statuses.is_empty() {
        for (key, total_value) in http_statuses {
            let total_value = total_value;
            let per_sec = total_value * 1000000 / duration;
            println!("   {key}:   total:{:<10} rate/sec:{:<10}", total_value, per_sec);
        }
    }

    // Errors
    let errors: Vec<_> = total.get_errors().iter().collect();
    if !errors.is_empty() {
        println!("─────────────────────────────────────────────────────────────────");
        println!(" Errors:                                                         ");
        for (key, total_value) in errors {
            let per_sec = *total_value * 1000000 / duration;
            let error_str = format!("{:?}", key);
            if error_str.len() > 42 {
                println!("   {}... {:>10} req/sec", &error_str[..42], per_sec);
            } else {
                println!("   {:<42} {:>10} req/sec", error_str, per_sec);
            }
        }
    }

    let pretty_lat = |l: f64| {
        if l >= 1_000_000.0 {
            format!("{:.2}s", l / 1_000_000.0)
        } else if l >= 1_000.0 {
            format!("{:.2}ms", l / 1_000.0)
        } else {
            format!("{:.2}µs", l)
        }
    };

    // Latency
    if total.latency.is_some() {
        println!("─────────────────────────────────────────────────────────────────");
        println!(" Latency:                                                   ");
        if let Some(latency) = total.latency {
            println!(
                "   min:{:<10} mean:{:<10} max:{:<10}",
                pretty_lat(latency.min() as f64),
                pretty_lat(latency.mean() as f64),
                pretty_lat(latency.max() as f64)
            );
            println!(
                "   p50:{:<10} p90:{:<10}  p99:{:<10}",
                pretty_lat(latency.value_at_quantile(0.50) as f64),
                pretty_lat(latency.value_at_quantile(0.95) as f64),
                pretty_lat(latency.value_at_quantile(0.99) as f64),
            );
        }
    }

    println!("─────────────────────────────────────────────────────────────────");

    Ok(())
}

fn runtime_engine(
    id: usize,
    opts: Options,
    rt_stats: Arc<Vec<RealtimeStats>>,
) -> Result<Statistics> {
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
            match opts.disable_lifo_slot {
                true => Builder::new_multi_thread()
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

                false => Builder::new_multi_thread()
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

    let mut stats = runtime.block_on(async { spawn_tasks(id, opts, rt_stats).await });

    stats.idle_time(
        total_park_time.load(Ordering::Relaxed).as_secs_f64() / start.elapsed().as_secs_f64(),
    );

    Ok(stats)
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
            ClientType::Hyper => {
                tasks.spawn(async move { http_hyper(id, con, opts, &stats[id]).await });
            }
            ClientType::HyperLegacy => {
                tasks.spawn(async move { http_hyper_legacy(id, con, opts, &stats[id]).await });
            }
            ClientType::HyperRt1 => {
                let con_client = client.as_ref().unwrap().clone();
                tasks.spawn(
                    async move { http_hyper_1rt(id, con, opts, con_client, &stats[id]).await },
                );
            }
            ClientType::HyperH2 => {
                tasks.spawn(async move { http_hyper_h2(id, con, opts, &stats[id]).await });
            }
            ClientType::Reqwest => {
                tasks.spawn(async move { http_reqwest(id, con, opts, &stats[id]).await });
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

    // Hide cursor and ensure it's restored on exit
    let _ = execute!(std::io::stdout(), cursor::Hide);

    defer! {
        let _ = execute!(std::io::stdout(), cursor::Show);
    }

    loop {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let mut total_ok = 0u64;
        let mut total_err = 0u64;
        let mut total_fail = 0u64;

        for stats in rt_stats.iter() {
            total_ok += stats.ok.swap(0, Ordering::Relaxed) as u64;
            total_err += stats.err.swap(0, Ordering::Relaxed) as u64;
            total_fail += stats.fail.swap(0, Ordering::Relaxed) as u64;
        }

        print!("\r{} Stats: ok: {total_ok}/sec, fail: {total_fail}/sec, err: {total_err}/sec",
               SPINNER[spinner_idx]);
        let _ = execute!(std::io::stdout(), terminal::Clear(terminal::ClearType::UntilNewLine));

        spinner_idx = (spinner_idx + 1) % SPINNER.len();
    }
}
