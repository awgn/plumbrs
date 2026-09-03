pub mod client;
pub mod engine;
pub mod metrics;
pub mod options;
pub mod stats;

use anyhow::{Result, anyhow};
use clap::Parser;
use client::ClientType;

use crossterm::{cursor, execute};

#[cfg(feature = "mimalloc")]
use mimalloc::MiMalloc;

use crate::options::Options;
use ctor::dtor;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[dtor]
fn cleanup() {
    _ = execute!(std::io::stderr(), cursor::Show);
}

fn main() -> Result<()> {
    // Hide cursor and ensure it's restored on exit
    _ = execute!(std::io::stderr(), cursor::Hide);

    #[cfg(feature = "mimalloc")]
    eprintln!("using allocator: mimalloc");
    #[cfg(not(feature = "mimalloc"))]
    eprintln!("using allocator: system");

    pretty_env_logger::init();
    let mut opts = Options::parse();
    check_options(&mut opts)?;
    engine::run_tokio_engines(opts)
}

fn check_options(opts: &mut Options) -> Result<()> {
    if matches!(opts.method, Some(http::Method::TRACE)) && opts.body.len() > 1 {
        return Err(anyhow!("TRACE method cannot have a body!"));
    }

    if !opts.trailers.is_empty() && opts.body.is_empty() {
        opts.body.push(String::new());
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

    if !opts.trailers.is_empty() {
        match opts.client_type {
            ClientType::Auto => opts.client_type = ClientType::HyperChunked,
            ClientType::HyperChunked | ClientType::HyperH2 => {}
            _ => {
                return Err(anyhow!(
                    "Trailers are only supported with hyper-chunked or hyper-h2 clients!"
                ));
            }
        }
    }

    match opts.client_type {
        #[cfg(all(target_os = "linux", feature = "tokio_uring"))]
        ClientType::TokioUring if opts.http2 => {
            return Err(anyhow!("HTTP/2 not supported with tokio-uring client!"));
        }
        #[cfg(all(target_os = "linux", feature = "tokio_uring"))]
        ClientType::TokioUring if opts.multithreaded.unwrap_or(1) > 1 => {
            return Err(anyhow!(
                "Multithreaded runtime not supported with io-uring client!"
            ));
        }
        #[cfg(all(target_os = "linux", feature = "tokio_uring"))]
        ClientType::TokioUring if opts.uri.is_empty() => {
            eprintln!("Missing URI. Try --help");
            std::process::exit(1);
        }
        ClientType::Auto
        | ClientType::HyperLegacy
        | ClientType::Hyper
        | ClientType::HyperRt1
        | ClientType::HyperH2
            if opts.uri.is_empty() =>
        {
            eprintln!("Missing URI. Try --help");
            std::process::exit(1);
        }

        ClientType::Reqwest if opts.sni.is_some() => {
            return Err(anyhow!("SNI option not available with reqwest client!"));
        }
        ClientType::HyperLegacy | ClientType::HyperRt1 | ClientType::Reqwest
            if opts.absolute_uri =>
        {
            return Err(anyhow!(
                "--absolute-uri is not available with this client!"
            ));
        }

        ClientType::Help => {
            println!("Available client types:");
            println!(
                "  hyper             - Hyper client, one per connection. Both HTTP/1 and HTTP/2. HTTPS"
            );
            #[cfg(feature = "mcp")]
            println!(
                "  hyper-mcp         - Hyper client for MCP servers, one per connection. Both HTTP/1 and HTTP/2. HTTPS"
            );
            println!(
                "  hyper-chunked     - Hyper client, one per connection, with multi-chunked body. Both HTTP/1 and HTTP/2. HTTPS"
            );
            println!(
                "  hyper-h2          - Hyper client, one per connection. Use h2 package, HTTP/2 only. HTTPS"
            );
            println!(
                "  hyper-legacy      - Hyper client (legacy), one per connection. Both HTTP/1 and HTTP/2"
            );
            println!(
                "  hyper-rt1         - Hyper client (legacy), one per runtime. Both HTTP/1 and HTTP/2"
            );
            println!(
                "  reqwest           - Reqwest client, one per runtime. Both HTTP/1 and HTTP/2. HTTPS"
            );
            #[cfg(all(target_os = "linux", feature = "tokio_uring"))]
            println!("  tokio-uring       - Tokio-uring client, one per thread. Only HTTP/1");
            #[cfg(all(target_os = "linux", feature = "monoio"))]
            println!("  monoio            - Monoio client, one per thread. Only HTTP/1");
            #[cfg(feature = "compio")]
            println!("  compio            - Compio client, one per thread. Only HTTP/1");
            std::process::exit(0);
        }
        _ => (),
    }



    for uri in &opts.uri {
        if let Ok(parsed) = uri.parse::<http::Uri>()
            && let Some(scheme) = parsed.scheme_str()
            && !scheme.eq_ignore_ascii_case("http")
            && !scheme.eq_ignore_ascii_case("https")
        {
            return Err(anyhow!(
                "Unsupported URI scheme '{scheme}' (expected http or https)"
            ));
        }
    }

    let has_https = opts.uri.iter().any(|uri| {
        uri.parse::<http::Uri>()
            .ok()
            .and_then(|u| u.scheme_str().map(|s| s.eq_ignore_ascii_case("https")))
            .unwrap_or(false)
    });

    if has_https {
        if !opts.client_type.supports_https() {
            return Err(anyhow!(
                "HTTPS is not supported with {} client!",
                opts.client_type
            ));
        }
        crate::client::tls::init(opts.insecure);
    } else if opts.sni.is_some() {
        return Err(anyhow!("--sni requires an https:// URI"));
    }

    if opts.sni.as_ref().is_some_and(|s| s.is_empty()) {
        return Err(anyhow!("SNI server name cannot be empty"));
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
