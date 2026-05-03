//! BTC up/down outcome ↔ BTC perp parity.
//!
//! Hyperliquid's first HIP-4 contracts settle on the BTC mark price at a
//! fixed UTC time (initially 06:00). Given the perp's mid `S`, the realised
//! volatility `σ` and the time-to-expiry `τ`, the risk-neutral probability of
//! `S_T ≥ K` is approximately
//!
//! ```text
//! P(S_T ≥ K) ≈ Φ( (ln(S/K) + (r − ½σ²) τ) / (σ √τ) )
//! ```
//!
//! Which under negligible drift over a 0–24h horizon collapses to
//!
//! ```text
//! P(S_T ≥ K) ≈ Φ( (ln(S/K) − ½σ²τ) / (σ √τ) )
//! ```
//!
//! When the YES token's mid prints meaningfully above (or below) this fair
//! value, the strategy fires:
//!   * if YES is rich vs the perp model => sell YES, hedge long BTC perp
//!   * if YES is cheap                  => buy YES, hedge short BTC perp
//!
//! The hedge ratio is the digital's delta `∂P/∂S = φ(d) / (S σ √τ)` rounded
//! to perp tick-size — essentially a tiny scalar of BTC notional per YES
//! token. The bot rebalances the perp leg every time the YES position drifts
//! by more than `delta_rebalance_thresh`.
use crate::common::{Quote, StrategyContext, StrategyEvent, StrategyId};
use crate::kelly::{binary_trade_size, KellyParams};
use hl_omm_core::{ClientOrderId, MarketKey, Price, Qty, Side, Venue};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use statrs::distribution::{ContinuousCDF, Normal};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, trace};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParityParams {
    /// Mispricing threshold in probability points (e.g. 0.01 = 1 %).
    pub min_edge: f64,
    /// Hard ceiling on YES tokens per signal (Kelly will usually pick less).
    pub max_qty: Decimal,
    /// Inventory size at which the perp delta-hedge rebalances.
    pub delta_rebalance_thresh: Decimal,
    /// Annualised volatility prior used as a fallback before enough samples.
    pub sigma_prior_annual: f64,
    /// Half-life (seconds) for the perp realised-vol estimator.
    pub vol_half_life_secs: f64,
    /// Total bot equity in USD — used by the Kelly sizer.
    pub equity_usd: Decimal,
    /// Fractional Kelly knobs.
    pub kelly: KellyParams,
}

impl Default for ParityParams {
    fn default() -> Self {
        Self {
            min_edge: 0.015,
            max_qty: dec!(50),
            delta_rebalance_thresh: dec!(5),
            sigma_prior_annual: 0.65,
            vol_half_life_secs: 90.0,
            equity_usd: dec!(50_000),
            kelly: KellyParams::default(),
        }
    }
}

#[derive(Default)]
struct VolEstimator {
    last_log: Option<f64>,
    last_ts: i64,
    ewma_var_per_sec: f64,
}

impl VolEstimator {
    /// Returns the annualised volatility.
    fn update(&mut self, mid: f64, ts_ns: i64, half_life_secs: f64) -> f64 {
        let log_mid = mid.ln();
        if let Some(prev) = self.last_log {
            let dt_secs = ((ts_ns - self.last_ts).max(1)) as f64 * 1e-9;
            if dt_secs > 0.0 {
                let r2 = (log_mid - prev).powi(2) / dt_secs;
                let alpha = 1.0 - 0.5_f64.powf(dt_secs / half_life_secs);
                self.ewma_var_per_sec =
                    (1.0 - alpha) * self.ewma_var_per_sec + alpha * r2;
            }
        }
        self.last_log = Some(log_mid);
        self.last_ts = ts_ns;
        // Annualised: var_per_sec * 31_536_000  ⇒ σ = √(var_per_sec) * √year
        (self.ewma_var_per_sec * 31_536_000.0).sqrt().max(0.05)
    }
}

pub struct BtcParity {
    pub params: ParityParams,
    pub yes_market: MarketKey,
    pub no_market: MarketKey,
    pub perp_market: MarketKey,
    pub strike: Decimal,
    pub expiry_ns: i64,
    vol: parking_lot::Mutex<VolEstimator>,
    yes_inv: parking_lot::Mutex<Decimal>,
    perp_hedge: parking_lot::Mutex<Decimal>,
    out: mpsc::UnboundedSender<StrategyEvent>,
    ctx: Arc<StrategyContext>,
    cloid_seq: parking_lot::Mutex<u128>,
}

impl BtcParity {
    pub fn new(
        params: ParityParams,
        yes_market: MarketKey,
        no_market: MarketKey,
        perp_market: MarketKey,
        strike: Decimal,
        expiry_ns: i64,
        ctx: Arc<StrategyContext>,
        out: mpsc::UnboundedSender<StrategyEvent>,
    ) -> Self {
        Self {
            params,
            yes_market,
            no_market,
            perp_market,
            strike,
            expiry_ns,
            vol: Default::default(),
            yes_inv: parking_lot::Mutex::new(Decimal::ZERO),
            perp_hedge: parking_lot::Mutex::new(Decimal::ZERO),
            out,
            ctx,
            cloid_seq: parking_lot::Mutex::new(0xBC_0000_0000_0000_0000_0000_0000_0000_u128),
        }
    }

    pub fn on_yes_inventory(&self, inv: Decimal) {
        *self.yes_inv.lock() = inv;
    }

    pub fn on_perp_inventory(&self, inv: Decimal) {
        *self.perp_hedge.lock() = inv;
    }

    pub fn on_tick(&self, now_ns: i64) {
        let perp_book = match self.ctx.book(&self.perp_market) {
            Some(b) => b,
            None => return,
        };
        let yes_book = match self.ctx.book(&self.yes_market) {
            Some(b) => b,
            None => return,
        };
        let perp_mid = match perp_book.microprice() {
            Some(p) => p.to_f64(),
            None => return,
        };
        let yes_mid = match yes_book.microprice() {
            Some(p) => p.to_f64(),
            None => return,
        };

        let mut sigma = self
            .vol
            .lock()
            .update(perp_mid, now_ns, self.params.vol_half_life_secs);
        if !sigma.is_finite() || sigma <= 0.0 {
            sigma = self.params.sigma_prior_annual;
        }

        let tau = ((self.expiry_ns - now_ns).max(0) as f64 * 1e-9) / 31_536_000.0;
        if tau <= 0.0 {
            return;
        }
        let strike = self.strike.to_f64().unwrap_or(0.0);
        let s = perp_mid;
        if strike <= 0.0 {
            return;
        }
        let d = ((s / strike).ln() - 0.5 * sigma * sigma * tau) / (sigma * tau.sqrt());
        let normal = Normal::new(0.0, 1.0).unwrap();
        let p_fair = normal.cdf(d);
        let phi_d = (1.0 / (2.0_f64 * std::f64::consts::PI).sqrt()) * (-0.5 * d * d).exp();
        let delta = phi_d / (s * sigma * tau.sqrt());

        let edge = yes_mid - p_fair;
        trace!(s, sigma, tau, p_fair, yes_mid, edge, "btc parity tick");

        if edge.abs() < self.params.min_edge {
            return;
        }

        // Edge > 0 => YES is rich, sell YES, long perp to hedge.
        let yes_side = if edge > 0.0 { Side::Sell } else { Side::Buy };
        let perp_side = if edge > 0.0 { Side::Buy } else { Side::Sell };

        let yes_px = yes_book
            .best_bid()
            .filter(|_| matches!(yes_side, Side::Sell))
            .map(|l| l.price.0)
            .or_else(|| yes_book.best_ask().map(|l| l.price.0))
            .unwrap_or(Decimal::from_f64_retain(yes_mid).unwrap_or(dec!(0.5)));

        // ---------- Kelly-optimal sizing ----------
        // We have an explicit point estimate (`p_fair`) and a market price
        // (`yes_mid`); the binary Kelly closed-form gives us the exact
        // log-optimal stake.
        let (signed_qty, kelly_f) =
            binary_trade_size(p_fair, yes_mid, self.params.equity_usd, &self.params.kelly);
        let qty = signed_qty.abs().min(self.params.max_qty);
        if qty <= Decimal::ZERO {
            return;
        }
        let perp_qty = (Decimal::try_from(delta).unwrap_or(Decimal::ZERO) * qty).round_dp(4);
        info!(kelly_f = kelly_f, qty = %qty, "btc parity Kelly sized");

        info!(edge = edge, p_fair = p_fair, yes_mid = yes_mid, "btc parity signal");

        let _ = self.out.send(StrategyEvent::Take(Quote {
            strategy: StrategyId::BtcParity,
            market: self.yes_market.clone(),
            side: yes_side,
            price: Price(yes_px),
            qty: Qty(qty),
            post_only: false,
            cloid: self.next_cloid(),
        }));
        let perp_px = perp_book
            .best_ask()
            .filter(|_| matches!(perp_side, Side::Buy))
            .map(|l| l.price.0)
            .or_else(|| perp_book.best_bid().map(|l| l.price.0))
            .unwrap_or(Decimal::from_f64_retain(perp_mid).unwrap_or(dec!(0)));
        if perp_qty.abs() >= dec!(0.0001) {
            let _ = self.out.send(StrategyEvent::Take(Quote {
                strategy: StrategyId::BtcParity,
                market: self.perp_market.clone(),
                side: perp_side,
                price: Price(perp_px),
                qty: Qty(perp_qty.abs()),
                post_only: false,
                cloid: self.next_cloid(),
            }));
        }
    }

    fn next_cloid(&self) -> ClientOrderId {
        let mut g = self.cloid_seq.lock();
        *g = g.wrapping_add(1);
        ClientOrderId::new(*g)
    }
}

#[allow(dead_code)]
const HL_PERP: Venue = Venue::HyperliquidPerp;
