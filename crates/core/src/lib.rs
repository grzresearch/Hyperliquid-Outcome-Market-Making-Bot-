//! Core domain types for the Hyperliquid outcome-market making bot.
//!
//! Everything in this crate is venue-neutral. Connectors translate venue
//! payloads into these types so that the strategy and risk layers can reason
//! about a unified, normalised picture across Hyperliquid HIP-4 outcome
//! markets, Polymarket binary CLOB markets, Kalshi event contracts, and
//! Hyperliquid BTC perps / spot.

pub mod book;
pub mod ids;
pub mod instrument;
pub mod metrics;
pub mod order;
pub mod position;
pub mod price;
pub mod time;

pub use book::{BookSide, BookUpdate, Level, OrderBook};
pub use ids::{ClientOrderId, InstrumentId, MarketKey, OutcomeKey, ThresholdDirection, Venue};
pub use instrument::{Instrument, InstrumentKind, OutcomeMarket, OutcomeSide, SettlementRule};
pub use order::{Order, OrderState, OrderType, Side, TimeInForce};
pub use position::{FillEvent, Position, PositionDelta};
pub use price::{Price, Probability, Qty};
pub use time::{now_micros, now_nanos, Timestamp};
