use crate::ids::MarketKey;
use crate::price::{Price, Qty};
use crate::time::Timestamp;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookSide {
    Bid,
    Ask,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct Level {
    pub price: Price,
    pub size: Qty,
}

#[derive(Clone, Debug)]
pub struct OrderBook {
    pub market: MarketKey,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    pub seq: u64,
    pub ts_recv_ns: Timestamp,
    pub ts_event_ns: Timestamp,
}

impl OrderBook {
    pub fn empty(market: MarketKey) -> Self {
        Self {
            market,
            bids: Vec::with_capacity(20),
            asks: Vec::with_capacity(20),
            seq: 0,
            ts_recv_ns: 0,
            ts_event_ns: 0,
        }
    }

    #[inline]
    pub fn best_bid(&self) -> Option<&Level> {
        self.bids.first()
    }

    #[inline]
    pub fn best_ask(&self) -> Option<&Level> {
        self.asks.first()
    }

    #[inline]
    pub fn mid(&self) -> Option<Price> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => {
                let mid = (b.price.0 + a.price.0) / rust_decimal::Decimal::TWO;
                Some(Price(mid))
            }
            _ => None,
        }
    }

    /// Microprice = (bid * ask_size + ask * bid_size) / (bid_size + ask_size).
    /// Standard size-weighted reference price used by execution models.
    pub fn microprice(&self) -> Option<Price> {
        let b = self.best_bid()?;
        let a = self.best_ask()?;
        let bs = b.size.0;
        let asz = a.size.0;
        let denom = bs + asz;
        if denom.is_zero() {
            return self.mid();
        }
        let num = b.price.0 * asz + a.price.0 * bs;
        Some(Price(num / denom))
    }

    pub fn spread(&self) -> Option<Price> {
        let b = self.best_bid()?;
        let a = self.best_ask()?;
        Some(Price(a.price.0 - b.price.0))
    }
}

/// Atomic snapshot wrapper used by strategies to read books without locking.
pub type BookHandle = Arc<RwLock<OrderBook>>;

#[derive(Clone, Debug)]
pub struct BookUpdate {
    pub market: MarketKey,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    pub seq: u64,
    pub ts_event_ns: Timestamp,
}
