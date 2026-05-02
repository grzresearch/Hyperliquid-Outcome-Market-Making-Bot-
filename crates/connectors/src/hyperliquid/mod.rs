//! Hyperliquid connector.
//!
//! The same gateway hosts perpetuals, spot, *and* the new HIP-4 outcome
//! markets — the only thing that changes per market is the `coin` field used
//! in subscriptions and order requests. HIP-4 outcome contracts use the
//! prefix `OUT:` (e.g. `OUT:BTC-78213-2026-05-03-YES`) in subscriptions and
//! settle in USDH; perps use the bare ticker (e.g. `BTC`) and settle in USDC.
//!
//! See: <https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket>
//! and the HIP-4 mainnet launch announcement (May 2 2026).
pub mod client;
pub mod outcome;
pub mod signing;
pub mod types;
pub mod ws;

pub use client::HyperliquidClient;
pub use outcome::{outcome_market_id, parse_outcome_market_id};

pub const REST_URL: &str = "https://api.hyperliquid.xyz";
pub const WS_URL: &str = "wss://api.hyperliquid.xyz/ws";
pub const TESTNET_REST_URL: &str = "https://api.hyperliquid-testnet.xyz";
pub const TESTNET_WS_URL: &str = "wss://api.hyperliquid-testnet.xyz/ws";
