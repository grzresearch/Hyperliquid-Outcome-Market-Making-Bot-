//! Risk gate. Every order produced by a strategy passes through here before
//! it is forwarded to a connector. Hard limits cause an outright reject;
//! soft limits trigger a size scale-down.
use dashmap::DashMap;
use hl_omm_core::{ClientOrderId, FillEvent, MarketKey, Order, Position, Side};
use parking_lot::RwLock;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskLimits {
    pub max_gross_notional_usd: Decimal,
    pub max_per_market_qty: Decimal,
    pub max_open_orders_per_market: usize,
    pub max_drawdown_usd: Decimal,
    pub stop_on_disconnect: bool,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_gross_notional_usd: dec!(50_000),
            max_per_market_qty: dec!(5000),
            max_open_orders_per_market: 16,
            max_drawdown_usd: dec!(2000),
            stop_on_disconnect: true,
        }
    }
}

pub struct RiskBook {
    pub limits: RwLock<RiskLimits>,
    positions: DashMap<MarketKey, Position>,
    open_orders: DashMap<MarketKey, usize>,
    realised_pnl: parking_lot::Mutex<Decimal>,
    peak_equity: parking_lot::Mutex<Decimal>,
    pub kill_switch: parking_lot::Mutex<bool>,
}

#[derive(Debug)]
pub enum RiskDecision {
    Pass,
    Resize(Decimal),
    Reject(&'static str),
}

impl RiskBook {
    pub fn new(limits: RiskLimits) -> Self {
        Self {
            limits: RwLock::new(limits),
            positions: DashMap::new(),
            open_orders: DashMap::new(),
            realised_pnl: parking_lot::Mutex::new(Decimal::ZERO),
            peak_equity: parking_lot::Mutex::new(Decimal::ZERO),
            kill_switch: parking_lot::Mutex::new(false),
        }
    }

    pub fn check(&self, order: &Order) -> RiskDecision {
        if *self.kill_switch.lock() {
            return RiskDecision::Reject("kill switch active");
        }
        let limits = self.limits.read();

        let open = self.open_orders.get(&order.market).map(|x| *x).unwrap_or(0);
        if open >= limits.max_open_orders_per_market {
            return RiskDecision::Reject("too many open orders");
        }

        let pos = self
            .positions
            .get(&order.market)
            .map(|p| p.qty)
            .unwrap_or(Decimal::ZERO);
        let direction = match order.side {
            Side::Buy => Decimal::ONE,
            Side::Sell => -Decimal::ONE,
        };
        let projected = pos + direction * order.qty.0;
        if projected.abs() > limits.max_per_market_qty {
            // Allow whatever fits inside the cap.
            let allowed = (limits.max_per_market_qty - pos.abs()).max(Decimal::ZERO);
            if allowed <= Decimal::ZERO {
                return RiskDecision::Reject("per-market cap reached");
            }
            return RiskDecision::Resize(allowed);
        }

        let drawdown = *self.peak_equity.lock() - *self.realised_pnl.lock();
        if drawdown > limits.max_drawdown_usd {
            warn!(?drawdown, "drawdown breached, killing");
            *self.kill_switch.lock() = true;
            return RiskDecision::Reject("max drawdown");
        }

        RiskDecision::Pass
    }

    pub fn on_order_acked(&self, market: &MarketKey) {
        let mut e = self.open_orders.entry(market.clone()).or_insert(0);
        *e += 1;
    }

    pub fn on_order_done(&self, market: &MarketKey) {
        if let Some(mut e) = self.open_orders.get_mut(market) {
            if *e > 0 {
                *e -= 1;
            }
        }
    }

    pub fn on_fill(&self, fill: &FillEvent) {
        let mut entry = self
            .positions
            .entry(fill.market.clone())
            .or_insert_with(|| Position::new(fill.market.clone()));
        let realised = entry.apply_fill(fill.side, fill.price.0, fill.qty.0, fill.ts_ns);
        let mut pnl = self.realised_pnl.lock();
        *pnl += realised;
        let mut peak = self.peak_equity.lock();
        if *pnl > *peak {
            *peak = *pnl;
        }
        debug!(?fill, realised = %realised, "risk book updated");
    }

    pub fn pnl(&self) -> Decimal {
        *self.realised_pnl.lock()
    }

    pub fn position(&self, market: &MarketKey) -> Option<Position> {
        self.positions.get(market).map(|p| p.value().clone())
    }

    pub fn snapshot(&self) -> RiskSnapshot {
        RiskSnapshot {
            realised_pnl: self.pnl(),
            peak_equity: *self.peak_equity.lock(),
            kill_switch: *self.kill_switch.lock(),
            positions: self
                .positions
                .iter()
                .map(|p| (p.key().clone(), p.value().clone()))
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskSnapshot {
    pub realised_pnl: Decimal,
    pub peak_equity: Decimal,
    pub kill_switch: bool,
    pub positions: Vec<(MarketKey, Position)>,
}
