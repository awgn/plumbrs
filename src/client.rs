pub mod hyper;
pub mod hyper_1rt;
pub mod hyper_h2;
pub mod hyper_legacy;
pub mod reqwest;
pub mod utils;
use clap::ValueEnum;

#[derive(ValueEnum, Debug, Copy, Clone)]
pub enum ClientType {
    HyperLegacy,
    Hyper1Rt,
    HyperH2,
    Hyper,
    Reqwest,
    Help,
}

impl std::fmt::Display for ClientType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientType::Reqwest => write!(f, "reqwest"),
            ClientType::HyperLegacy => write!(f, "hyper-legacy"),
            ClientType::Hyper1Rt => write!(f, "hyper-1rt"),
            ClientType::HyperH2 => write!(f, "hyper-h2"),
            ClientType::Hyper => write!(f, "hyper"),
            ClientType::Help => write!(f, "help"),
        }
    }
}
