//! Polymarket connector. Targets the CLOB REST + WebSocket APIs documented
//! at <https://docs.polymarket.com>.
//!
//! Polymarket uses a Polygon-based smart contract for settlement, but for the
//! purposes of *trading* the bot only talks to:
//!   * Gamma market data API: `https://gamma-api.polymarket.com`
//!   * CLOB API:              `https://clob.polymarket.com`
//!   * Real-time stream:      `wss://ws-subscriptions-clob.polymarket.com/ws/`
//!
//! Authentication on Polymarket is layered: an EIP-712 signature derives an
//! API key triple (key/secret/passphrase). After that signing is HMAC-SHA256.
pub mod client;
pub mod ws;

pub const GAMMA_URL: &str = "https://gamma-api.polymarket.com";
pub const CLOB_URL: &str = "https://clob.polymarket.com";
pub const WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
pub const WS_USER_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/user";
