use hl_omm_core::{BookUpdate, ClientOrderId, FillEvent, MarketKey, Order, OrderState, Venue};
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Debug, Error)]
pub enum ConnectorError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("auth: {0}")]
    Auth(String),
    #[error("decoding: {0}")]
    Decoding(String),
    #[error("rate limited")]
    RateLimited,
    #[error("rejected: {0}")]
    Rejected(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Events emitted by every connector into the strategy layer.
#[derive(Debug, Clone)]
pub enum ConnectorEvent {
    Book(BookUpdate),
    Trade {
        market: MarketKey,
        price: hl_omm_core::Price,
        qty: hl_omm_core::Qty,
        side: hl_omm_core::Side,
        ts_event_ns: i64,
    },
    OrderUpdate {
        cloid: ClientOrderId,
        venue: Venue,
        state: OrderState,
        venue_oid: Option<String>,
    },
    Fill(FillEvent),
    /// The venue session has dropped and is reconnecting; downstream consumers
    /// should treat in-flight orders as "unknown" until the resync completes.
    Resyncing { venue: Venue },
    Resynced { venue: Venue },
}

/// Handle returned by `Connector::start` — gives the strategy a way to push
/// orders and a stream of events back from the venue.
pub struct ConnectorHandle {
    pub venue: Venue,
    pub events: mpsc::UnboundedReceiver<ConnectorEvent>,
    pub commands: mpsc::UnboundedSender<ConnectorCommand>,
}

#[derive(Debug, Clone)]
pub enum ConnectorCommand {
    Subscribe(MarketKey),
    Unsubscribe(MarketKey),
    Place(Order),
    Cancel { market: MarketKey, cloid: ClientOrderId },
    CancelAll { market: MarketKey },
}
