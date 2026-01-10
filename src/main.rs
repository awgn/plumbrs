pub mod client;
pub mod engine;
pub mod metrics;
pub mod options;
pub mod stats;

use anyhow::{Result, anyhow};
use clap::Parser;
use client::ClientType;

use crossterm::{cursor, execute};
#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

use crate::options::Options;
use ctor::dtor;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

#[dtor]
fn cleanup() {
    _ = execute!(std::io::stdout(), cursor::Show);
}

fn main() -> Result<()> {
    // Hide cursor and ensure it's restored on exit
    _ = execute!(std::io::stdout(), cursor::Hide);

    pretty_env_logger::init();
    let mut opts = Options::parse();
    check_options(&mut opts)?;
    engine::run_tokio_engines(opts)
}

fn check_options(opts: &mut Options) -> Result<()> {
    if matches!(opts.method, Some(http::Method::TRACE)) && opts.body.len() > 1 {
        return Err(anyhow!("TRACE method cannot have a body!"));
    }

    if opts.method.is_none() {
        if opts.body.is_empty() {
            opts.method = Some(http::Method::GET);
        } else {
            opts.method = Some(http::Method::POST);
        }
    }

    #[cfg(feature = "mcp")]
    if !matches!(opts.client_type, ClientType::Auto)
        && !matches!(opts.client_type, ClientType::HyperMcp)
        && (opts.mcp || opts.mcp_sse)
    {
        return Err(anyhow!("MCP not supported with this client!"));
    }

    match opts.client_type {
        #[cfg(all(target_os = "linux", feature = "io_uring"))]
        ClientType::IoUring if opts.http2 => {
            return Err(anyhow!("HTTP/2 not supported with io-uring client!"));
        }
        #[cfg(all(target_os = "linux", feature = "io_uring"))]
        ClientType::IoUring if opts.multithreaded.unwrap_or(1) > 1 => {
            return Err(anyhow!(
                "Multithreaded runtime not supported with io-uring client!"
            ));
        }
        #[cfg(all(target_os = "linux", feature = "io_uring"))]
        ClientType::IoUring if opts.uri.is_empty() => {
            println!("Missing URI. Try --help");
            std::process::exit(1);
        }
        ClientType::Auto
        | ClientType::HyperLegacy
        | ClientType::Hyper
        | ClientType::HyperRt1
        | ClientType::HyperH2
            if opts.uri.is_empty() =>
        {
            println!("Missing URI. Try --help");
            std::process::exit(1);
        }
        ClientType::HyperLegacy | ClientType::HyperRt1 if opts.host.is_some() => {
            return Err(anyhow!("Host option not available with this client!"));
        }
        ClientType::Reqwest if !opts.trailers.is_empty() => {
            return Err(anyhow!("Trailers not supported with reqwest client!"));
        }
        ClientType::Help => {
            println!("Available client types:");
            println!(
                "  hyper             - Hyper client, one per connection. Both HTTP/1 and HTTP/2"
            );
            println!(
                "  hyper-multichunk  - Hyper client, one per connection, with multi-chunked body. Both HTTP/1 and HTTP/2"
            );
            println!(
                "  hyper-h2          - Hyper client, one per connection. Use h2 package, HTTP/2 only"
            );
            println!(
                "  hyper-legacy      - Hyper client (legacy), one per connection. Both HTTP/1 and HTTP/2"
            );
            println!(
                "  hyper-rt1         - Hyper client (legacy), one per runtime. Both HTTP/1 and HTTP/2"
            );
            println!(
                "  reqwest           - Reqwest client, one per runtime. Both HTTP/1 and HTTP/2"
            );
            println!("  io-uring          - IO-uring client, one per thread. Only HTTP/1");
            std::process::exit(0);
        }
        _ => (),
    }

    if let Some(nt) = opts.multithreaded
        && !opts.threads.is_multiple_of(nt)
    {
        return Err(anyhow!(
            "The number of threads must be an exact multiple of the thread count for each individual runtime"
        ));
    }

    Ok(())
}
