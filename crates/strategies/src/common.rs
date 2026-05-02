use hl_omm_connectors::common::ConnectorCommand;
use hl_omm_core::{ClientOrderId, MarketKey, OrderBook, Price, Qty, Side, Venue};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum StrategyId {
    AvellanedaStoikov,
    CrossVenueArb,
    BtcParity,
}

impl StrategyId {
    pub fn as_str(&self) -> &'static str {
        match self {
            StrategyId::AvellanedaStoikov => "avellaneda_stoikov",
            StrategyId::CrossVenueArb => "cross_venue_arb",
            StrategyId::BtcParity => "btc_parity",
        }
    }
}

/// Outbound quote produced by a strategy. The execution layer dedupes by
/// `cloid` so re-submitting the same quote unchanged is a no-op.
#[derive(Clone, Debug)]
pub struct Quote {
    pub strategy: StrategyId,
    pub market: MarketKey,
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    pub post_only: bool,
    pub cloid: ClientOrderId,
}

#[derive(Clone, Debug)]
pub enum StrategyEvent {
    Quote(Quote),
    Cancel { market: MarketKey, cloid: ClientOrderId },
    /// Take liquidity now (used by arb / parity strategies).
    Take(Quote),
}

/// Shared strategy context — read-only handles to the live book cache plus
/// command channels into each connector. Strategies pull market state through
/// this object and emit `StrategyEvent`s.
pub struct StrategyContext {
    pub books: Arc<RwLock<HashMap<MarketKey, OrderBook>>>,
    pub commands: HashMap<Venue, tokio::sync::mpsc::UnboundedSender<ConnectorCommand>>,
}

impl StrategyContext {
    pub fn new(
        commands: HashMap<Venue, tokio::sync::mpsc::UnboundedSender<ConnectorCommand>>,
    ) -> Self {
        Self {
            books: Arc::new(RwLock::new(HashMap::new())),
            commands,
        }
    }

    pub fn book(&self, market: &MarketKey) -> Option<OrderBook> {
        self.books.read().get(market).cloned()
    }
}
