//! Lightweight metrics counters consumed by the dashboard and (optionally) by
//! Prometheus. Hot-path code should call the inline functions below; they
//! bottom out in `metrics` macros so the recording layer is a single atomic
//! increment when no exporter is attached.
use metrics::{counter, gauge, histogram};

#[inline(always)]
pub fn inc_md_event(venue: &'static str) {
    counter!("md_events_total", "venue" => venue).increment(1);
}

#[inline(always)]
pub fn record_md_latency_us(venue: &'static str, micros: f64) {
    histogram!("md_latency_us", "venue" => venue).record(micros);
}

#[inline(always)]
pub fn record_order_rtt_us(venue: &'static str, micros: f64) {
    histogram!("order_rtt_us", "venue" => venue).record(micros);
}

#[inline(always)]
pub fn record_pnl(strategy: &'static str, value: f64) {
    gauge!("strategy_pnl_usd", "strategy" => strategy).set(value);
}

#[inline(always)]
pub fn inc_strategy_signal(strategy: &'static str) {
    counter!("strategy_signals_total", "strategy" => strategy).increment(1);
}
