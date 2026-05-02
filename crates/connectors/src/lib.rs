//! Venue connectors. Each venue has its own module wrapping the REST + WS API
//! and translating wire messages to the `hl-omm-core` domain types.
//!
//! Connectors expose a uniform [`Connector`] trait so the strategy layer can
//! be venue-agnostic. The trait is intentionally narrow: subscribe to a
//! market, place / cancel orders, pump events. Anything more exotic (margin
//! transfers, USDH minting) is exposed through venue-specific helpers.
pub mod common;
pub mod hyperliquid;
pub mod kalshi;
pub mod polymarket;

pub use common::{ConnectorEvent, ConnectorHandle, ConnectorError};
