//! Helpers for the HIP-4 outcome-market ticker layout.
//!
//! Hyperliquid identifies outcome legs with the form
//!   `OUT:<UNDERLYING>-<STRIKE_INT>-<YYYY-MM-DD>-<YES|NO>`
//! For example, the BTC daily binary that settles at May 3 2026 06:00 UTC
//! against $78,213 has tickers
//!   `OUT:BTC-78213-2026-05-03-YES`
//!   `OUT:BTC-78213-2026-05-03-NO`
//!
//! The settlement value is paid in USDH (HL's native stablecoin issued by
//! Native Markets following the validator vote in late 2025).

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use hl_omm_core::{
    InstrumentKind, MarketKey, OutcomeKey, OutcomeMarket, OutcomeSide, SettlementRule,
    ThresholdDirection, Venue,
};
use rust_decimal::Decimal;
use std::str::FromStr;

#[derive(Clone, Debug)]
pub struct OutcomeTicker {
    pub underlying: String,
    pub strike: Decimal,
    pub expiry: DateTime<Utc>,
    pub side: OutcomeSide,
}

pub fn outcome_market_id(t: &OutcomeTicker) -> String {
    let strike = t.strike.trunc().to_string();
    let date = t.expiry.format("%Y-%m-%d");
    let side = match t.side {
        OutcomeSide::Yes => "YES",
        OutcomeSide::No => "NO",
    };
    format!("OUT:{}-{}-{}-{}", t.underlying, strike, date, side)
}

pub fn parse_outcome_market_id(s: &str) -> Option<OutcomeTicker> {
    let rest = s.strip_prefix("OUT:")?;
    let mut parts = rest.rsplitn(5, '-');
    let side = parts.next()?;
    let day = parts.next()?;
    let month = parts.next()?;
    let year = parts.next()?;
    let head = parts.next()?;
    let mut head_parts = head.rsplitn(2, '-');
    let strike = head_parts.next()?;
    let underlying = head_parts.next()?;

    let date = NaiveDateTime::parse_from_str(
        &format!("{}-{}-{} 06:00:00", year, month, day),
        "%Y-%m-%d %H:%M:%S",
    )
    .ok()?;
    let expiry = Utc.from_utc_datetime(&date);
    let strike = Decimal::from_str(strike).ok()?;

    let side = match side {
        "YES" => OutcomeSide::Yes,
        "NO" => OutcomeSide::No,
        _ => return None,
    };

    Some(OutcomeTicker {
        underlying: underlying.to_string(),
        strike,
        expiry,
        side,
    })
}

/// Build a normalised [`OutcomeMarket`] from a ticker plus tick / size info
/// (which the bot pulls from `outcomeMeta`).
pub fn build_outcome_market(
    ticker: &OutcomeTicker,
    direction: ThresholdDirection,
    tick_size: Decimal,
    min_size: Decimal,
) -> OutcomeMarket {
    let id = outcome_market_id(ticker);
    let key = MarketKey::new(Venue::HyperliquidOutcome, id);
    let outcome = OutcomeKey {
        underlying: ticker.underlying.clone(),
        strike_cents: (ticker.strike * Decimal::from(100))
            .trunc()
            .to_string()
            .parse()
            .unwrap_or(0),
        expiry_ns: ticker.expiry.timestamp_nanos_opt().unwrap_or(0),
        direction,
    };
    OutcomeMarket {
        key,
        outcome,
        side: ticker.side,
        settlement: SettlementRule {
            underlying: ticker.underlying.clone(),
            strike: ticker.strike,
            direction,
            expiry_ns: ticker.expiry.timestamp_nanos_opt().unwrap_or(0),
        },
        tick_size,
        min_size,
    }
}

#[allow(dead_code)]
pub const HIP4_INSTRUMENT_KIND: InstrumentKind = InstrumentKind::HyperliquidOutcome;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_ticker() {
        let raw = "OUT:BTC-78213-2026-05-03-YES";
        let parsed = parse_outcome_market_id(raw).expect("parse");
        assert_eq!(parsed.underlying, "BTC");
        assert_eq!(parsed.strike, Decimal::from(78_213));
        assert_eq!(parsed.side, OutcomeSide::Yes);
        assert_eq!(outcome_market_id(&parsed), raw);
    }
}
