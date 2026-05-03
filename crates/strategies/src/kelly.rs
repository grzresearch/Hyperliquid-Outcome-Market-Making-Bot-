//! Kelly-criterion position sizing.
//!
//! The (full) Kelly fraction maximises the long-run logarithmic growth rate
//! of equity. For our three strategies we use three specialised forms:
//!
//! 1. **Binary outcome arb / parity (`kelly_binary`)** — when we have a
//!    point estimate `p_true` of the true YES probability and we can buy
//!    YES at `p_market` (or sell YES at `p_market`):
//!
//!    ```text
//!    f* = (p_true − p_market) / (p_market · (1 − p_market))
//!    ```
//!
//!    Derivation: a 1-USDH bet on YES at price `b = p_market` returns
//!    `(1 - b)/b` units per USDH staked if YES, loses 1 if NO. With true
//!    probability `p`, full Kelly is
//!      `f* = (b·p − (1 − p)) / b · (1/(1−b))  =  (p − b) / (b(1 − b))`.
//!
//! 2. **Continuous Kelly (`kelly_continuous`)** — for the market-making
//!    strategy and for any signal whose edge is approximately Gaussian
//!    with edge `μ` and dispersion `σ`:
//!
//!    ```text
//!    f* = μ / σ²
//!    ```
//!
//!    Equivalent to the maximum-Sharpe scaling for log-utility.
//!
//! 3. **Cross-venue arb (`kelly_arb`)** — the round-trip profit per USD
//!    risked is `edge_bps / 10000`, with bounded variance from the slippage
//!    on either leg. We use a closed-form quadratic Kelly that takes
//!    fee_bps + bridging into account.
//!
//! All three are scaled by a configurable `fraction` (typical: `0.25` for
//! "quarter Kelly"), clamped to a hard `max_fraction`, and snapped to the
//! venue tick size before being returned to the strategy.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KellyParams {
    /// Fraction of the full Kelly bet to actually take (≤ 1.0). Reduces
    /// drawdown in exchange for lower long-run growth — half-Kelly gives
    /// ~75 % of full-Kelly growth at half the variance.
    pub fraction: f64,
    /// Hard cap on the fraction of equity per trade.
    pub max_fraction: f64,
    /// Minimum size to bother emitting an order at all.
    pub min_qty: Decimal,
    /// Round results down to this many decimal places (matches venue tick).
    pub size_decimals: u32,
}

impl Default for KellyParams {
    fn default() -> Self {
        Self {
            fraction: 0.25,
            max_fraction: 0.10,
            min_qty: dec!(1),
            size_decimals: 2,
        }
    }
}

/// Kelly fraction for a binary outcome bet — buying YES at `p_market` when
/// our point estimate is `p_true`. Returns a *signed* fraction:
///   * positive = take the YES side (buy YES at p_market)
///   * negative = take the NO side  (sell YES at p_market, equivalent to
///     buying NO at 1 − p_market)
#[inline]
pub fn kelly_binary(p_true: f64, p_market: f64) -> f64 {
    let p = p_true.clamp(1e-6, 1.0 - 1e-6);
    let b = p_market.clamp(1e-6, 1.0 - 1e-6);
    (p - b) / (b * (1.0 - b))
}

/// Continuous-time Kelly: `μ / σ²`. `mu` is the expected per-period return
/// of the strategy (fraction of stake), `sigma` its per-period std dev.
#[inline]
pub fn kelly_continuous(mu: f64, sigma: f64) -> f64 {
    if sigma <= 0.0 {
        return 0.0;
    }
    mu / (sigma * sigma)
}

/// Kelly fraction for a fee-aware cross-venue arb. `edge_bps` is the gross
/// edge (rich − cheap), `cost_bps` is the all-in round-trip cost
/// (maker/taker fees on both venues + bridging). `var_bps` approximates the
/// variance of the realised edge once both legs settle (typically driven by
/// the worse-side slippage and the latency window between legs).
pub fn kelly_arb(edge_bps: f64, cost_bps: f64, var_bps: f64) -> f64 {
    let mu = (edge_bps - cost_bps) / 10_000.0;
    let sigma2 = (var_bps / 10_000.0).powi(2).max(1e-9);
    if mu <= 0.0 {
        return 0.0;
    }
    mu / sigma2
}

/// Apply fractional + capped Kelly to a notional and return the order qty
/// (in units of the asset's base size).
///
/// `equity_usd`: total bot equity in USD. `price`: the per-unit price of
/// the leg being sized. The result is rounded down to `size_decimals` and
/// clamped to `[min_qty, equity_usd · max_fraction / price]`.
pub fn size_from_kelly(
    raw_kelly: f64,
    equity_usd: Decimal,
    price: Decimal,
    p: &KellyParams,
) -> Decimal {
    if !raw_kelly.is_finite() || raw_kelly == 0.0 {
        return Decimal::ZERO;
    }
    let scaled = (raw_kelly.abs() * p.fraction).min(p.max_fraction);
    if scaled <= 0.0 {
        return Decimal::ZERO;
    }
    let scaled = Decimal::from_f64_retain(scaled).unwrap_or(Decimal::ZERO);
    let notional = equity_usd * scaled;
    if price <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let qty = (notional / price).round_dp(p.size_decimals);
    if qty < p.min_qty {
        return Decimal::ZERO;
    }
    qty
}

/// Convenience: full pipeline for a binary outcome trade. Returns
/// (signed_qty, kelly_fraction). `signed_qty > 0` ⇒ buy YES;
/// `< 0` ⇒ sell YES (or equivalently buy NO at `1 − p_market`).
pub fn binary_trade_size(
    p_true: f64,
    p_market: f64,
    equity_usd: Decimal,
    p: &KellyParams,
) -> (Decimal, f64) {
    let f = kelly_binary(p_true, p_market);
    let qty = size_from_kelly(
        f,
        equity_usd,
        Decimal::from_f64_retain(p_market.max(1e-3)).unwrap_or(dec!(0.5)),
        p,
    );
    (if f >= 0.0 { qty } else { -qty }, f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_kelly_long_when_underpriced() {
        // True YES probability 60 %, market is offering YES at 50 %.
        let f = kelly_binary(0.60, 0.50);
        assert!(f > 0.0);
        // 0.10 / (0.5 * 0.5) = 0.40
        assert!((f - 0.40).abs() < 1e-9);
    }

    #[test]
    fn binary_kelly_short_when_overpriced() {
        let f = kelly_binary(0.40, 0.55);
        assert!(f < 0.0);
    }

    #[test]
    fn binary_kelly_zero_when_fair() {
        let f = kelly_binary(0.42, 0.42);
        assert!(f.abs() < 1e-12);
    }

    #[test]
    fn continuous_kelly_basic() {
        // μ = 1%, σ = 5% ⇒ f* = 0.01/0.0025 = 4.0
        let f = kelly_continuous(0.01, 0.05);
        assert!((f - 4.0).abs() < 1e-9);
    }

    #[test]
    fn arb_kelly_skips_negative_edge() {
        let f = kelly_arb(20.0, 50.0, 30.0);
        assert_eq!(f, 0.0);
    }

    #[test]
    fn size_respects_caps_and_min() {
        let p = KellyParams { fraction: 1.0, max_fraction: 0.05, min_qty: dec!(1), size_decimals: 2 };
        // 4× Kelly raw, capped at 5% of 10,000 USD = 500 USD; at 0.5 USDH/YES
        // that's 1000 YES tokens.
        let q = size_from_kelly(4.0, dec!(10000), dec!(0.5), &p);
        assert_eq!(q, dec!(1000));
    }

    #[test]
    fn size_returns_zero_below_min() {
        let p = KellyParams { fraction: 0.01, max_fraction: 1.0, min_qty: dec!(50), size_decimals: 0 };
        let q = size_from_kelly(0.05, dec!(10), dec!(1), &p);
        assert_eq!(q, Decimal::ZERO);
    }
}
