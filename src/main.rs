pub mod client;
pub mod engine;
pub mod options;
pub mod stats;

use anyhow::{Result, anyhow};
use clap::Parser;
use client::ClientType;

#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

use crate::options::Options;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

fn main() -> Result<()> {
    pretty_env_logger::init();
    let opts = Options::parse();
    enforce_consistency(&opts)?;
    engine::run_tokio_engines(opts)
}

fn enforce_consistency(opts: &Options) -> Result<()> {
    #[cfg(all(not(tokio_unstable), feature = "tokio_metrics"))]
    compile_error!("Tokio metrics requires tokio_unstable build!");

    #[cfg(feature = "orion_client")]
    if opts.http2_can_share && !opts.http2 {
        return Err(anyhow!(
            "HTTP2 (--http2) must be enabled to allow connection sharing!"
        ));
    }

    #[cfg(feature = "orion_client")]
    if opts.http2_can_share
        && !matches!(opts.client_type, ClientType::HyperLegacy)
        && !matches!(opts.client_type, ClientType::Hyper1Rt)
    {
        return Err(anyhow!(
            "HTTP2 Connection sharing only available with hyper-legacy or hyper-1rt client!"
        ));
    }

    match opts.client_type {
        ClientType::HyperLegacy
        | ClientType::Hyper
        | ClientType::Hyper1Rt
        | ClientType::HyperH2
            if opts.uri.is_empty() =>
        {
            eprintln!("HTTP uri not specified!");
            std::process::exit(1);
        }
        ClientType::HyperLegacy | ClientType::Hyper1Rt if opts.host.is_some() => {
            return Err(anyhow!("Host option not available with this client!"));
        }
        ClientType::Help => {
            println!("Available client types:");
            println!("  hyper         - Hyper client, one per connection. Both HTTP/1 and HTTP/2");
            println!(
                "  hyper-h2      - Hyper client, one per connection. Use h2 package, HTTP/2 only"
            );
            println!(
                "  hyper-legacy  - Hyper client (legacy), one per connection. Both HTTP/1 and HTTP/2"
            );
            println!(
                "  hyper-1rt     - Hyper client (legacy), one per runtime. Both HTTP/1 and HTTP/2"
            );
            println!("  reqwest       - Reqwest client, one per runtime. Both HTTP/1 and HTTP/2");
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
