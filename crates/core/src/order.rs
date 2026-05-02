use crate::ids::{ClientOrderId, MarketKey};
use crate::price::{Price, Qty};
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    #[inline]
    pub fn flip(self) -> Self {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }

    #[inline]
    pub fn sign(self) -> i8 {
        match self {
            Side::Buy => 1,
            Side::Sell => -1,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderType {
    Limit,
    PostOnly,
    Ioc,
    Market,
    /// Hyperliquid "ALO" (add-liquidity-only) — same idea as post-only.
    Alo,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Alo,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    Pending,
    Open,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Order {
    pub cloid: ClientOrderId,
    pub market: MarketKey,
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    pub filled: Qty,
    pub order_type: OrderType,
    pub tif: TimeInForce,
    pub state: OrderState,
    pub created_ns: Timestamp,
    pub updated_ns: Timestamp,
    /// Venue-side order id (oid for Hyperliquid, order_id for Polymarket / Kalshi).
    pub venue_oid: Option<String>,
}

impl Order {
    pub fn new_limit(
        cloid: ClientOrderId,
        market: MarketKey,
        side: Side,
        price: Price,
        qty: Qty,
        post_only: bool,
        now: Timestamp,
    ) -> Self {
        let (order_type, tif) = if post_only {
            (OrderType::Alo, TimeInForce::Alo)
        } else {
            (OrderType::Limit, TimeInForce::Gtc)
        };
        Self {
            cloid,
            market,
            side,
            price,
            qty,
            filled: Qty::ZERO,
            order_type,
            tif,
            state: OrderState::Pending,
            created_ns: now,
            updated_ns: now,
            venue_oid: None,
        }
    }
}
