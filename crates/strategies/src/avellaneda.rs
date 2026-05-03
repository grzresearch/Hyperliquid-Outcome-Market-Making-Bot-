//! Avellaneda-Stoikov market maker, adapted to binary outcome markets.
//!
//! Continuous-time HJB solution from Avellaneda & Stoikov (2008) gives a
//! reservation price `r` and an optimal half-spread `δ`:
//!
//! ```text
//! r(s, q, t) = s − q · γ · σ² · (T − t)
//! δ(t)       = γ · σ² · (T − t) + (1/γ) · ln(1 + γ/k)
//! bid        = r − δ                  ask = r + δ
//! ```
//!
//! For HIP-4 outcome tokens we replace the unbounded mid `s` with the YES
//! microprice — already a probability in `[0, 1]` — and clamp the resulting
//! quotes into `[tick, 1 − tick]`. Inventory `q` is the outstanding YES
//! position; its drift in probability space is bounded by `[0, 1]` so the
//! same Brownian assumption is locally fine on minute-to-hour horizons.
use crate::common::{Quote, StrategyContext, StrategyEvent, StrategyId};
use crate::kelly::{kelly_continuous, size_from_kelly, KellyParams};
use hl_omm_core::{
    ClientOrderId, InstrumentKind, MarketKey, OrderBook, Price, Qty, Side, Venue,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::trace;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AvellanedaParams {
    /// Risk aversion (γ). Higher = tighter inventory control, wider spreads.
    pub gamma: f64,
    /// Order arrival intensity (κ in the AS paper). Higher = thinner book =>
    /// post tighter to capture flow.
    pub kappa: f64,
    /// Minimum quoted size (in YES tokens).
    pub min_size: Decimal,
    /// Maximum absolute inventory the maker will hold.
    pub max_inventory: Decimal,
    /// EMA half-life used to estimate σ (in seconds).
    pub vol_half_life_secs: f64,
    /// Tick rounding for posted prices.
    pub tick: Decimal,
    /// Total bot equity in USD — drives the Kelly cap on quoted size.
    pub equity_usd: Decimal,
    /// Fractional Kelly knobs for the per-quote size cap.
    pub kelly: KellyParams,
}

impl Default for AvellanedaParams {
    fn default() -> Self {
        Self {
            gamma: 0.6,
            kappa: 1.5,
            min_size: dec!(10),
            max_inventory: dec!(2000),
            vol_half_life_secs: 60.0,
            tick: dec!(0.001),
            equity_usd: dec!(50_000),
            kelly: KellyParams::default(),
        }
    }
}

#[derive(Default)]
struct VolEstimator {
    last_mid: Option<f64>,
    last_ts_ns: i64,
    ewma_var: f64,
}

impl VolEstimator {
    fn update(&mut self, mid: f64, ts_ns: i64, half_life_secs: f64) -> f64 {
        if let Some(prev) = self.last_mid {
            let dt_secs = ((ts_ns - self.last_ts_ns).max(1)) as f64 * 1e-9;
            let r = (mid - prev) / dt_secs.sqrt().max(1e-9);
            let alpha = 1.0 - (-(dt_secs.ln_1p()) / half_life_secs).exp().min(1.0);
            let alpha = alpha.clamp(1e-4, 1.0);
            self.ewma_var = (1.0 - alpha) * self.ewma_var + alpha * r * r;
        }
        self.last_mid = Some(mid);
        self.last_ts_ns = ts_ns;
        self.ewma_var.sqrt()
    }
}

pub struct AvellanedaStoikov {
    pub params: AvellanedaParams,
    pub yes_market: MarketKey,
    pub no_market: MarketKey,
    pub expiry_ns: i64,
    vol: parking_lot::Mutex<VolEstimator>,
    inventory: parking_lot::Mutex<Decimal>,
    out: mpsc::UnboundedSender<StrategyEvent>,
    ctx: Arc<StrategyContext>,
    cloid_seq: parking_lot::Mutex<u128>,
}

impl AvellanedaStoikov {
    pub fn new(
        params: AvellanedaParams,
        yes_market: MarketKey,
        no_market: MarketKey,
        expiry_ns: i64,
        ctx: Arc<StrategyContext>,
        out: mpsc::UnboundedSender<StrategyEvent>,
    ) -> Self {
        Self {
            params,
            yes_market,
            no_market,
            expiry_ns,
            vol: Default::default(),
            inventory: parking_lot::Mutex::new(Decimal::ZERO),
            out,
            ctx,
            cloid_seq: parking_lot::Mutex::new(0xA5A5_0000_0000_0000_0000_0000_0000_0000),
        }
    }

    pub fn on_inventory(&self, inv: Decimal) {
        *self.inventory.lock() = inv;
    }

    /// Driven by every YES book update.
    pub fn on_book(&self, book: &OrderBook, now_ns: i64) {
        if book.market != self.yes_market {
            return;
        }
        let micro = match book.microprice() {
            Some(p) => p.to_f64(),
            None => return,
        };
        let sigma = self.vol.lock().update(micro, now_ns, self.params.vol_half_life_secs);

        let t_remaining_secs = ((self.expiry_ns - now_ns).max(0) as f64) * 1e-9;
        let normalised_t = (t_remaining_secs / 86_400.0).clamp(1e-3, 1.0);

        // Reservation shift in probability units.
        let q = self.inventory.lock().to_f64().unwrap_or(0.0);
        let inv_skew = q * self.params.gamma * sigma * sigma * normalised_t;
        let reservation = (micro - inv_skew).clamp(0.001, 0.999);

        let half_spread = self.params.gamma * sigma * sigma * normalised_t
            + (1.0 / self.params.gamma) * (1.0 + self.params.gamma / self.params.kappa).ln();
        let half_spread = half_spread.max(self.params.tick.to_f64().unwrap_or(0.001));

        let bid = (reservation - half_spread).clamp(0.001, 0.999);
        let ask = (reservation + half_spread).clamp(0.001, 0.999);

        let bid = self.snap(bid);
        let ask = self.snap(ask);
        if bid >= ask {
            return;
        }

        // Inventory throttle — pull the side that would push us further over
        // the cap.
        let cap = self.params.max_inventory;
        let post_bid = q < cap.to_f64().unwrap_or(0.0);
        let post_ask = -q < cap.to_f64().unwrap_or(0.0);

        // ---------- Kelly sizing per quote ----------
        // The maker's per-fill expected return is the captured half-spread
        // δ, with realised dispersion σ_per_sec · √(expected fill latency).
        // We approximate the latency by 1 second; this gives a Kelly
        // fraction of δ / σ². The result is converted to a YES-token qty
        // via the configured equity, then floored to `min_size`.
        let mu_per_fill = half_spread.max(1e-9);
        let sigma_per_fill = sigma.max(1e-9);
        let kelly_f = kelly_continuous(mu_per_fill, sigma_per_fill);
        let kelly_qty = size_from_kelly(
            kelly_f,
            self.params.equity_usd,
            Decimal::from_f64_retain(reservation).unwrap_or(dec!(0.5)),
            &self.params.kelly,
        );
        let qty = if kelly_qty > self.params.min_size { kelly_qty } else { self.params.min_size };

        if post_bid {
            self.emit(self.yes_market.clone(), Side::Buy, bid, qty);
        }
        if post_ask {
            self.emit(self.yes_market.clone(), Side::Sell, ask, qty);
        }
        // Mirror on NO leg using the YES↔NO parity (price_no = 1 − price_yes).
        // This adds redundant maker depth — the two legs can both fill on the
        // same trade because they share the same matching engine.
        if post_bid {
            self.emit(self.no_market.clone(), Side::Sell, dec!(1) - bid, qty);
        }
        if post_ask {
            self.emit(self.no_market.clone(), Side::Buy, dec!(1) - ask, qty);
        }

        trace!(?micro, ?sigma, ?reservation, ?bid, ?ask, "AS quotes");
    }

    fn snap(&self, p: f64) -> Decimal {
        let tick = self.params.tick.to_f64().unwrap_or(0.001);
        let snapped = (p / tick).round() * tick;
        Decimal::try_from(snapped).unwrap_or(self.params.tick)
    }

    fn next_cloid(&self) -> ClientOrderId {
        let mut g = self.cloid_seq.lock();
        *g = g.wrapping_add(1);
        ClientOrderId::new(*g)
    }

    fn emit(&self, market: MarketKey, side: Side, price: Decimal, qty: Decimal) {
        let _ = self.out.send(StrategyEvent::Quote(Quote {
            strategy: StrategyId::AvellanedaStoikov,
            market,
            side,
            price: Price(price),
            qty: Qty(qty),
            post_only: true,
            cloid: self.next_cloid(),
        }));
    }
}

#[allow(dead_code)]
const KIND: InstrumentKind = InstrumentKind::HyperliquidOutcome;
#[allow(dead_code)]
const VENUE: Venue = Venue::HyperliquidOutcome;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_rounds_to_tick() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let ctx = Arc::new(StrategyContext::new(Default::default()));
        let s = AvellanedaStoikov::new(
            AvellanedaParams::default(),
            MarketKey::new(Venue::HyperliquidOutcome, "X-YES"),
            MarketKey::new(Venue::HyperliquidOutcome, "X-NO"),
            0,
            ctx,
            tx,
        );
        assert_eq!(s.snap(0.5234), dec!(0.523));
    }

    #[test]
    fn ema_handles_zero_dt() {
        let mut v = VolEstimator::default();
        let s1 = v.update(0.5, 1_000, 60.0);
        let s2 = v.update(0.5, 1_000, 60.0);
        assert!(s1.is_finite());
        assert!(s2.is_finite());
    }
}
