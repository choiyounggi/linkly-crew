use std::time::Duration;

/// Bus server configuration — DESIGN.md §3/§11 M2, plan B1.
#[derive(Debug, Clone)]
pub struct BusConfig {
    pub token: String,
    pub max_rounds: u8,
    pub max_delivery_attempts: u32,
    pub retry_base: Duration,
    pub ping_interval: Duration,
    pub outbound_buffer: usize,
    pub seen_capacity: usize,
}

impl BusConfig {
    /// Shipped defaults per plan B1: max_rounds=3, max_delivery_attempts=3,
    /// retry_base=250ms, ping_interval=30s, outbound_buffer=64,
    /// seen_capacity=4096. Tests override `retry_base` (and other fields)
    /// via struct-update syntax.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            max_rounds: 3,
            max_delivery_attempts: 3,
            retry_base: Duration::from_millis(250),
            ping_interval: Duration::from_secs(30),
            outbound_buffer: 64,
            seen_capacity: 4096,
        }
    }
}
