//! Kalshi connector. Targets the v2 trade API documented at
//! <https://docs.kalshi.com>.
//!
//! Authentication on Kalshi uses RSA-PSS: the user uploads a public key, then
//! signs every request with `RSA-PSS(SHA-256, MGF1, salt=32)`. The signature
//! is sent in the `KALSHI-ACCESS-SIGNATURE` header along with a millisecond
//! timestamp, the path, and the HTTP method. Session tokens — used for the
//! WebSocket — expire every 30 minutes.
pub mod client;
pub mod ws;

pub const REST_URL: &str = "https://api.kalshi.com/trade-api/v2";
pub const WS_URL: &str = "wss://api.kalshi.com/trade-api/ws/v2";
