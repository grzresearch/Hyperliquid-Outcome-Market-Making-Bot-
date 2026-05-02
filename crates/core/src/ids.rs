use serde::{Deserialize, Serialize};
use std::fmt;

/// Trading venues the bot speaks to.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Venue {
    #[default]
    HyperliquidOutcome,
    HyperliquidPerp,
    HyperliquidSpot,
    Polymarket,
    Kalshi,
}

impl fmt::Display for Venue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Venue::HyperliquidOutcome => "hl-outcome",
            Venue::HyperliquidPerp => "hl-perp",
            Venue::HyperliquidSpot => "hl-spot",
            Venue::Polymarket => "polymarket",
            Venue::Kalshi => "kalshi",
        };
        f.write_str(s)
    }
}

/// Venue-local instrument identifier (e.g. "BTC-78213-2026-05-03" or a Polymarket condition ID).
#[derive(Clone, Debug, Eq, PartialEq, Hash, Default, Serialize, Deserialize)]
pub struct InstrumentId(pub String);

impl InstrumentId {
    #[inline]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// (venue, instrument) tuple — globally unique key into book / order tables.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Default, Serialize, Deserialize)]
pub struct MarketKey {
    pub venue: Venue,
    pub instrument: InstrumentId,
}

impl MarketKey {
    pub fn new(venue: Venue, instrument: impl Into<String>) -> Self {
        Self {
            venue,
            instrument: InstrumentId::new(instrument),
        }
    }
}

impl fmt::Display for MarketKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}::{}", self.venue, self.instrument)
    }
}

/// Logical outcome identifier shared across venues — for example
/// `OutcomeKey { underlying: "BTC", strike_cents: 7_821_300, expiry_ts_ns: ... }`
/// is the same logical event whether it trades on Hyperliquid, Polymarket or Kalshi.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct OutcomeKey {
    pub underlying: String,
    pub strike_cents: i64,
    pub expiry_ns: i64,
    pub direction: ThresholdDirection,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdDirection {
    Above,
    Below,
}

/// Bot-side client order id (cloid). Hyperliquid expects a 16-byte hex value;
/// we format it that way on the wire but keep a u128 internally for speed.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ClientOrderId(pub u128);

impl ClientOrderId {
    #[inline]
    pub fn new(v: u128) -> Self {
        Self(v)
    }

    #[inline]
    pub fn to_hex(&self) -> String {
        format!("0x{:032x}", self.0)
    }
}

impl fmt::Display for ClientOrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}
