#[cfg(feature = "compio")]
pub mod compio;
pub mod hyper;
pub mod hyper_chunked;
pub mod hyper_h2;
pub mod hyper_legacy;
#[cfg(feature = "mcp")]
pub mod hyper_mcp;
pub mod hyper_rt1;
#[cfg(all(target_os = "linux", feature = "monoio"))]
pub mod monoio;
pub mod reqwest;
#[cfg(all(target_os = "linux", feature = "tokio_uring"))]
pub mod tokio_uring;
pub mod utils;
use clap::ValueEnum;

#[derive(ValueEnum, Debug, Copy, Clone)]
pub enum ClientType {
    Auto,
    #[cfg(feature = "mcp")]
    HyperMcp,
    HyperLegacy,
    HyperChunked,
    HyperRt1,
    HyperH2,
    Hyper,
    Reqwest,
    #[cfg(all(target_os = "linux", feature = "tokio_uring"))]
    TokioUring,
    #[cfg(all(target_os = "linux", feature = "monoio"))]
    Monoio,
    #[cfg(feature = "compio")]
    Compio,
    Help,
}

impl std::fmt::Display for ClientType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientType::Auto => write!(f, "auto"),
            ClientType::Hyper => write!(f, "hyper"),
            ClientType::HyperLegacy => write!(f, "hyper-legacy"),
            ClientType::HyperChunked => write!(f, "hyper-chunked"),
            ClientType::HyperRt1 => write!(f, "hyper-rt1"),
            ClientType::HyperH2 => write!(f, "hyper-h2"),
            #[cfg(feature = "mcp")]
            ClientType::HyperMcp => write!(f, "hyper-mcp"),
            ClientType::Reqwest => write!(f, "reqwest"),
            #[cfg(all(target_os = "linux", feature = "tokio_uring"))]
            ClientType::TokioUring => write!(f, "tokio-uring"),
            #[cfg(all(target_os = "linux", feature = "monoio"))]
            ClientType::Monoio => write!(f, "monoio"),
            #[cfg(feature = "compio")]
            ClientType::Compio => write!(f, "compio"),
            ClientType::Help => write!(f, "help"),
        }
    }
}
