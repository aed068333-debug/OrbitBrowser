use crate::errors::{AppError, AppResult};
use std::net::TcpListener;
use std::time::{SystemTime, UNIX_EPOCH};

const PORT_MIN: u16 = 41_000;
const PORT_MAX: u16 = 45_000;
const ALLOCATE_ATTEMPTS: u16 = 128;

pub fn allocate() -> AppResult<u16> {
    let span = (PORT_MAX - PORT_MIN) as u64;
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let mut candidate = PORT_MIN.saturating_add((seed % (span + 1)) as u16);

    for _ in 0..ALLOCATE_ATTEMPTS {
        if is_available(candidate) {
            return Ok(candidate);
        }
        candidate = if candidate >= PORT_MAX {
            PORT_MIN
        } else {
            candidate.saturating_add(1)
        };
    }

    portpicker::pick_unused_port().ok_or_else(|| {
        AppError::new("proxy_core_start_failed", "No available local proxy port").retryable(true)
    })
}

fn is_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}
