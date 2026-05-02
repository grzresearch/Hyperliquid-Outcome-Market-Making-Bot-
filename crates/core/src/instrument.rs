use crate::ids::{InstrumentId, MarketKey, OutcomeKey, ThresholdDirection, Venue};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentKind {
    /// Hyperliquid HIP-4 outcome token (YES or NO leg).
    HyperliquidOutcome,
    /// Polymarket binary outcome token.
    PolymarketBinary,
    /// Kalshi event contract.
    KalshiBinary,
    /// BTC perp linear contract on Hyperliquid (used by the parity strategy).
    Perp,
    /// BTC spot pair on Hyperliquid (also used by parity).
    Spot,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeSide {
    Yes,
    No,
}

impl OutcomeSide {
    pub fn flip(self) -> Self {
        match self {
            OutcomeSide::Yes => OutcomeSide::No,
            OutcomeSide::No => OutcomeSide::Yes,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettlementRule {
    pub underlying: String,
    pub strike: Decimal,
    pub direction: ThresholdDirection,
    pub expiry_ns: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutcomeMarket {
    pub key: MarketKey,
    /// Logical event identifier — same value across venues for the same event.
    pub outcome: OutcomeKey,
    pub side: OutcomeSide,
    pub settlement: SettlementRule,
    /// Tick size in price units (e.g. 0.001 USDH).
    pub tick_size: Decimal,
    /// Minimum order size in base units (YES/NO tokens).
    pub min_size: Decimal,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instrument {
    pub key: MarketKey,
    pub kind: InstrumentKind,
    pub tick_size: Decimal,
    pub min_size: Decimal,
    pub size_decimals: u32,
    pub price_decimals: u32,
    /// Filled in for outcome instruments only.
    pub outcome: Option<OutcomeMarket>,
}

impl Instrument {
    pub fn perp(coin: &str, tick: Decimal, min: Decimal) -> Self {
        Self {
            key: MarketKey::new(Venue::HyperliquidPerp, coin.to_string()),
            kind: InstrumentKind::Perp,
            tick_size: tick,
            min_size: min,
            size_decimals: 4,
            price_decimals: 1,
            outcome: None,
        }
    }

    pub fn spot(coin: &str, tick: Decimal, min: Decimal) -> Self {
        Self {
            key: MarketKey::new(Venue::HyperliquidSpot, coin.to_string()),
            kind: InstrumentKind::Spot,
            tick_size: tick,
            min_size: min,
            size_decimals: 4,
            price_decimals: 1,
            outcome: None,
        }
    }

    pub fn outcome(market: OutcomeMarket, kind: InstrumentKind) -> Self {
        Self {
            key: market.key.clone(),
            kind,
            tick_size: market.tick_size,
            min_size: market.min_size,
            size_decimals: 2,
            price_decimals: 4,
            outcome: Some(market),
        }
    }

    #[inline]
    pub fn id(&self) -> &InstrumentId {
        &self.key.instrument
    }

    #[inline]
    pub fn venue(&self) -> Venue {
        self.key.venue
    }
}
