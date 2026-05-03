//! Cross-venue arbitrage between Hyperliquid HIP-4 outcome tokens, Polymarket
//! binary markets and Kalshi event contracts.
//!
//! The model is straightforward: each venue prices the same logical event in
//! `[0, 1]`. After fees and slippage, if the cheapest YES anywhere is below
//! the most expensive YES anywhere by more than the round-trip cost, we lift
//! the cheap leg and short / sell the rich leg. The same logic applies to
//! the NO side via `p_no = 1 − p_yes`.
//!
//! Special care:
//! - Polymarket trades on Polygon and settles in USDC; gas + bridging is
//!   amortised here as a basis-points haircut.
//! - Kalshi quotes prices in integer cents; spreads of <2 ¢ are usually
//!   uneconomical.
//! - Hyperliquid charges no maker fee for opening / minting outcome tokens,
//!   only on close. The bot prefers the maker side on Hyperliquid wherever
//!   possible.
use crate::common::{Quote, StrategyContext, StrategyEvent, StrategyId};
use crate::kelly::{kelly_arb, size_from_kelly, KellyParams};
use hl_omm_core::{ClientOrderId, MarketKey, OutcomeKey, OutcomeSide, Price, Qty, Side, Venue};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::info;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArbParams {
    pub min_edge_bps: i64,
    pub max_notional_usd: Decimal,
    pub max_slippage_bps: i64,
    pub fee_bps_per_venue: HashMap<String, i64>,
    pub bridging_bps_polygon: i64,
    /// Total bot equity in USD — used by the Kelly sizer.
    pub equity_usd: Decimal,
    /// Fractional Kelly (0.25 = quarter-Kelly).
    pub kelly: KellyParams,
    /// Approximate edge variance in bps (drives the Kelly denominator).
    pub edge_var_bps: f64,
}

impl Default for ArbParams {
    fn default() -> Self {
        let mut fees = HashMap::new();
        fees.insert("hl-outcome".into(), -25); // maker rebate, AS docs
        fees.insert("polymarket".into(), 50);
        fees.insert("kalshi".into(), 70);
        Self {
            min_edge_bps: 35,
            max_notional_usd: dec!(2000),
            max_slippage_bps: 20,
            fee_bps_per_venue: fees,
            bridging_bps_polygon: 15,
            equity_usd: dec!(50_000),
            kelly: KellyParams::default(),
            edge_var_bps: 80.0,
        }
    }
}

/// Linked legs for the same outcome event. Strategy spawn-time decides which
/// venues are listed; missing entries are simply ignored.
#[derive(Clone, Debug)]
pub struct VenueLeg {
    pub venue: Venue,
    pub yes_market: MarketKey,
    pub no_market: MarketKey,
}

pub struct CrossVenueArb {
    pub params: ArbParams,
    pub event: OutcomeKey,
    pub legs: Vec<VenueLeg>,
    out: mpsc::UnboundedSender<StrategyEvent>,
    ctx: Arc<StrategyContext>,
    cloid_seq: parking_lot::Mutex<u128>,
}

#[derive(Copy, Clone, Debug)]
struct Quotes {
    bid: Decimal,
    ask: Decimal,
    bid_size: Decimal,
    ask_size: Decimal,
}

impl CrossVenueArb {
    pub fn new(
        params: ArbParams,
        event: OutcomeKey,
        legs: Vec<VenueLeg>,
        ctx: Arc<StrategyContext>,
        out: mpsc::UnboundedSender<StrategyEvent>,
    ) -> Self {
        Self {
            params,
            event,
            legs,
            out,
            ctx,
            cloid_seq: parking_lot::Mutex::new(0xCC_0000_0000_0000_0000_0000_0000_0000_u128),
        }
    }

    /// Re-evaluate the arb every time any leg's book updates.
    pub fn on_tick(&self) {
        let snaps: Vec<(VenueLeg, Quotes)> = self
            .legs
            .iter()
            .filter_map(|leg| self.snapshot(leg).map(|q| (leg.clone(), q)))
            .collect();
        if snaps.len() < 2 {
            return;
        }

        // Find the cheapest ask (where to BUY YES) and the richest bid
        // (where to SELL YES) across legs.
        let (buy_idx, _) = snaps
            .iter()
            .enumerate()
            .min_by(|a, b| a.1 .1.ask.cmp(&b.1 .1.ask))
            .unwrap();
        let (sell_idx, _) = snaps
            .iter()
            .enumerate()
            .max_by(|a, b| a.1 .1.bid.cmp(&b.1 .1.bid))
            .unwrap();
        if buy_idx == sell_idx {
            return;
        }
        let (buy_leg, buy_q) = &snaps[buy_idx];
        let (sell_leg, sell_q) = &snaps[sell_idx];

        let buy_fee_bps = self.fee_bps(buy_leg.venue) + self.bridge_bps(buy_leg.venue);
        let sell_fee_bps = self.fee_bps(sell_leg.venue) + self.bridge_bps(sell_leg.venue);
        let total_cost_bps = buy_fee_bps + sell_fee_bps;

        let edge_bps = ((sell_q.bid - buy_q.ask) * dec!(10000) / buy_q.ask).round();
        let net_edge_bps = edge_bps.to_string().parse::<i64>().unwrap_or(0) - total_cost_bps;
        if net_edge_bps < self.params.min_edge_bps {
            return;
        }

        // ---------- Kelly-optimal sizing ----------
        // Treat the realised edge as Gaussian with mean `net_edge_bps` and
        // std-dev `edge_var_bps`. Continuous Kelly gives the optimal share
        // of equity to stake; we then clip to top-of-book depth.
        let kelly_f = kelly_arb(
            edge_bps.to_string().parse::<f64>().unwrap_or(0.0),
            total_cost_bps as f64,
            self.params.edge_var_bps,
        );
        let kelly_qty = size_from_kelly(
            kelly_f,
            self.params.equity_usd,
            buy_q.ask,
            &self.params.kelly,
        );
        let depth_qty = buy_q.ask_size.min(sell_q.bid_size);
        let cap_qty = self.params.max_notional_usd / buy_q.ask.max(dec!(0.01));
        let qty = depth_qty.min(cap_qty).min(kelly_qty);
        if qty <= Decimal::ZERO {
            return;
        }

        info!(
            event = ?self.event,
            buy = %buy_leg.venue,
            sell = %sell_leg.venue,
            buy_px = %buy_q.ask,
            sell_px = %sell_q.bid,
            edge_bps = edge_bps.to_string(),
            net_bps = net_edge_bps,
            kelly_f = kelly_f,
            qty = %qty,
            "x-venue arb signal"
        );

        // Hit/lift both legs simultaneously.
        let _ = self.out.send(StrategyEvent::Take(Quote {
            strategy: StrategyId::CrossVenueArb,
            market: buy_leg.yes_market.clone(),
            side: Side::Buy,
            price: Price(buy_q.ask),
            qty: Qty(qty),
            post_only: false,
            cloid: self.next_cloid(),
        }));
        let _ = self.out.send(StrategyEvent::Take(Quote {
            strategy: StrategyId::CrossVenueArb,
            market: sell_leg.yes_market.clone(),
            side: Side::Sell,
            price: Price(sell_q.bid),
            qty: Qty(qty),
            post_only: false,
            cloid: self.next_cloid(),
        }));
    }

    fn snapshot(&self, leg: &VenueLeg) -> Option<Quotes> {
        let book = self.ctx.book(&leg.yes_market)?;
        let bid = book.best_bid()?;
        let ask = book.best_ask()?;
        Some(Quotes {
            bid: bid.price.0,
            ask: ask.price.0,
            bid_size: bid.size.0,
            ask_size: ask.size.0,
        })
    }

    fn fee_bps(&self, venue: Venue) -> i64 {
        *self
            .params
            .fee_bps_per_venue
            .get(venue.to_string().as_str())
            .unwrap_or(&50)
    }

    fn bridge_bps(&self, venue: Venue) -> i64 {
        if matches!(venue, Venue::Polymarket) {
            self.params.bridging_bps_polygon
        } else {
            0
        }
    }

    fn next_cloid(&self) -> ClientOrderId {
        let mut g = self.cloid_seq.lock();
        *g = g.wrapping_add(1);
        ClientOrderId::new(*g)
    }
}

#[allow(dead_code)]
const SIDE_YES: OutcomeSide = OutcomeSide::Yes;
