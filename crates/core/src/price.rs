use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Price expressed in quote units (USDH for Hyperliquid outcome / USDC for
/// Polymarket / USD for Kalshi). For binary outcome contracts the price is in
/// `[0, 1]` and equals the implied probability of YES; we still keep it in
/// `Decimal` because consistency across venues outweighs the small cost.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Price(pub Decimal);

impl Price {
    pub const ZERO: Price = Price(Decimal::ZERO);
    pub const ONE: Price = Price(Decimal::ONE);

    #[inline]
    pub fn new(d: Decimal) -> Self {
        Self(d)
    }

    #[inline]
    pub fn from_f64(v: f64) -> Self {
        Self(Decimal::from_f64_retain(v).unwrap_or(Decimal::ZERO))
    }

    #[inline]
    pub fn to_f64(&self) -> f64 {
        use rust_decimal::prelude::ToPrimitive;
        self.0.to_f64().unwrap_or(0.0)
    }
}

/// Implied probability of YES — alias kept for readability in strategy code.
pub type Probability = Price;

/// Quantity. For outcome markets it is the number of YES (or NO) tokens; for
/// perp it is contracts (BTC). We keep this in `Decimal` for safety.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Qty(pub Decimal);

impl Qty {
    pub const ZERO: Qty = Qty(Decimal::ZERO);

    #[inline]
    pub fn new(d: Decimal) -> Self {
        Self(d)
    }

    #[inline]
    pub fn to_f64(&self) -> f64 {
        use rust_decimal::prelude::ToPrimitive;
        self.0.to_f64().unwrap_or(0.0)
    }
}
