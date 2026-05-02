use crate::ids::{ClientOrderId, MarketKey};
use crate::order::Side;
use crate::price::{Price, Qty};
use crate::time::Timestamp;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Position {
    pub market: MarketKey,
    pub qty: Decimal,
    pub avg_entry: Decimal,
    pub realised_pnl: Decimal,
    pub fees_paid: Decimal,
    pub unrealised_pnl: Decimal,
    pub last_update_ns: Timestamp,
}

impl Position {
    pub fn new(market: MarketKey) -> Self {
        Self {
            market,
            ..Default::default()
        }
    }

    /// Apply a fill, returning the realised pnl that resulted from it.
    pub fn apply_fill(&mut self, side: Side, price: Decimal, qty: Decimal, ts: Timestamp) -> Decimal {
        let signed = match side {
            Side::Buy => qty,
            Side::Sell => -qty,
        };
        let new_qty = self.qty + signed;
        let mut realised = Decimal::ZERO;

        // Reducing or flipping the position realises pnl.
        if !self.qty.is_zero() && self.qty.is_sign_positive() != signed.is_sign_positive() {
            let closing_qty = signed.abs().min(self.qty.abs());
            let direction = if self.qty.is_sign_positive() {
                Decimal::ONE
            } else {
                -Decimal::ONE
            };
            realised = (price - self.avg_entry) * closing_qty * direction;
            self.realised_pnl += realised;
            // If we closed past zero, reset avg_entry to the new fill price.
            if signed.abs() > self.qty.abs() {
                self.avg_entry = price;
            }
        } else if self.qty.is_zero() {
            self.avg_entry = price;
        } else {
            // Adding to an existing position: weighted average.
            let total_cost = self.avg_entry * self.qty.abs() + price * qty;
            self.avg_entry = total_cost / (self.qty.abs() + qty);
        }

        self.qty = new_qty;
        self.last_update_ns = ts;
        realised
    }

    pub fn mark_to(&mut self, price: Decimal) {
        self.unrealised_pnl = (price - self.avg_entry) * self.qty;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FillEvent {
    pub cloid: ClientOrderId,
    pub market: MarketKey,
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    pub fee: Decimal,
    pub ts_ns: Timestamp,
    /// Hyperliquid returns liquidity = "M" for maker, "T" for taker.
    pub is_maker: bool,
}

#[derive(Clone, Debug)]
pub struct PositionDelta {
    pub market: MarketKey,
    pub qty_change: Decimal,
}
