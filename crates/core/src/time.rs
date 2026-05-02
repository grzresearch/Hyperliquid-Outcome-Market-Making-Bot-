use std::time::{SystemTime, UNIX_EPOCH};

pub type Timestamp = i64;

#[inline(always)]
pub fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

#[inline(always)]
pub fn now_micros() -> i64 {
    now_nanos() / 1_000
}

#[inline(always)]
pub fn now_millis() -> i64 {
    now_nanos() / 1_000_000
}
