//! Strategy layer.
//!
//! Three signals run side-by-side, sharing the same book cache and risk gate:
//!
//! 1. [`avellaneda::AvellanedaStoikov`] - inventory-aware spread market making
//!    on the Hyperliquid HIP-4 outcome books (YES + NO).
//! 2. [`xvenue_arb::CrossVenueArb`] - looks for the same logical event listed
//!    on Polymarket / Kalshi / Hyperliquid and trades the divergence.
//! 3. [`btc_parity::BtcParity`] - prices the binary BTC up/down outcome
//!    against the BTC perp's implied probability; trades the basis.
pub mod avellaneda;
pub mod btc_parity;
pub mod common;
pub mod kelly;
pub mod xvenue_arb;

pub use avellaneda::AvellanedaStoikov;
pub use btc_parity::BtcParity;
pub use common::{Quote, StrategyContext, StrategyEvent, StrategyId};
pub use kelly::{
    binary_trade_size, kelly_arb, kelly_binary, kelly_continuous, size_from_kelly, KellyParams,
};
pub use xvenue_arb::CrossVenueArb;
