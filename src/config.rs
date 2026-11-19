use std::{
    env,
    net::{AddrParseError, SocketAddr},
};

pub(crate) fn performance_baseline() -> bool {
    env::var("MUSE_PERF_PROFILE").is_ok_and(|value| value == "baseline")
}

pub struct Config {
    pub bind_addr: SocketAddr,
}

impl Config {
    pub fn from_env() -> Result<Self, AddrParseError> {
        let bind_addr = env::var("BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:3000".into())
            .parse()?;
        Ok(Self { bind_addr })
    }
}
