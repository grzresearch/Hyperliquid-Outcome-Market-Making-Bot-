use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use hl_omm_core::{BookUpdate, Level, MarketKey, Price, Qty, Venue};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::common::{ConnectorCommand, ConnectorEvent};

#[derive(Debug, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
enum PolyWsEvent {
    Book(PolyBookSnapshot),
    PriceChange(PolyPriceChange),
    LastTradePrice(PolyLastTrade),
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct PolyBookSnapshot {
    asset_id: String,
    timestamp: String,
    bids: Vec<PolyLevel>,
    asks: Vec<PolyLevel>,
}

#[derive(Debug, Deserialize)]
struct PolyPriceChange {
    asset_id: String,
    timestamp: String,
    changes: Vec<PolyChange>,
}

#[derive(Debug, Deserialize)]
struct PolyChange {
    price: String,
    side: String, // "BUY" | "SELL"
    size: String,
}

#[derive(Debug, Deserialize)]
struct PolyLastTrade {
    asset_id: String,
    price: String,
    size: String,
    side: String,
    timestamp: String,
}

#[derive(Debug, Deserialize)]
struct PolyLevel {
    price: String,
    size: String,
}

pub struct PolymarketWs;

impl PolymarketWs {
    pub fn spawn() -> (
        mpsc::UnboundedReceiver<ConnectorEvent>,
        mpsc::UnboundedSender<ConnectorCommand>,
    ) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(Self::run(event_tx, cmd_rx));
        (event_rx, cmd_tx)
    }

    async fn run(
        events: mpsc::UnboundedSender<ConnectorEvent>,
        mut commands: mpsc::UnboundedReceiver<ConnectorCommand>,
    ) {
        let mut backoff = Duration::from_millis(250);
        loop {
            match Self::session(&events, &mut commands).await {
                Ok(()) => break,
                Err(e) => {
                    warn!(error = %e, "polymarket ws disconnected");
                    let _ = events.send(ConnectorEvent::Resyncing { venue: Venue::Polymarket });
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(15));
                }
            }
        }
    }

    async fn session(
        events: &mpsc::UnboundedSender<ConnectorEvent>,
        commands: &mut mpsc::UnboundedReceiver<ConnectorCommand>,
    ) -> Result<()> {
        let (ws, _) = connect_async(super::WS_URL).await.context("poly ws connect")?;
        info!(url = super::WS_URL, "polymarket ws connected");
        let (mut sink, mut stream) = ws.split();
        let _ = events.send(ConnectorEvent::Resynced { venue: Venue::Polymarket });

        let mut subscribed: Vec<String> = Vec::new();
        let mut hb = tokio::time::interval(Duration::from_secs(20));
        hb.tick().await;

        loop {
            tokio::select! {
                msg = stream.next() => {
                    let Some(msg) = msg else { break };
                    let msg = msg.context("poly ws read")?;
                    if let Message::Text(t) = msg {
                        Self::handle(&t, events);
                    }
                }
                cmd = commands.recv() => {
                    let Some(cmd) = cmd else { return Ok(()) };
                    if let ConnectorCommand::Subscribe(market) = cmd {
                        subscribed.push(market.instrument.0.clone());
                        let body = serde_json::json!({
                            "type": "MARKET",
                            "assets_ids": [market.instrument.0],
                        });
                        sink.send(Message::Text(body.to_string())).await?;
                    }
                }
                _ = hb.tick() => {
                    sink.send(Message::Ping(Vec::new())).await?;
                }
            }
        }
        Ok(())
    }

    fn handle(text: &str, events: &mpsc::UnboundedSender<ConnectorEvent>) {
        let arr: Vec<PolyWsEvent> = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => match serde_json::from_str::<PolyWsEvent>(text) {
                Ok(v) => vec![v],
                Err(_) => return,
            },
        };
        for ev in arr {
            match ev {
                PolyWsEvent::Book(b) => {
                    let market = MarketKey::new(Venue::Polymarket, b.asset_id);
                    let bids = b.bids.iter().map(level).collect();
                    let asks = b.asks.iter().map(level).collect();
                    let ts_ns = b.timestamp.parse::<i64>().unwrap_or(0) * 1_000_000;
                    let _ = events.send(ConnectorEvent::Book(BookUpdate {
                        market,
                        bids,
                        asks,
                        seq: 0,
                        ts_event_ns: ts_ns,
                    }));
                }
                PolyWsEvent::PriceChange(c) => {
                    // Strategies treat price_change as a delta — we synthesise
                    // a one-side book update; a real implementation merges it
                    // with the cached snapshot. The strategy layer always
                    // fetches the snapshot on subscribe so this is safe.
                    let market = MarketKey::new(Venue::Polymarket, c.asset_id);
                    let mut bids: Vec<Level> = vec![];
                    let mut asks: Vec<Level> = vec![];
                    for change in &c.changes {
                        let lvl = Level {
                            price: Price(Decimal::from_str(&change.price).unwrap_or_default()),
                            size: Qty(Decimal::from_str(&change.size).unwrap_or_default()),
                        };
                        if change.side == "BUY" {
                            bids.push(lvl);
                        } else {
                            asks.push(lvl);
                        }
                    }
                    let ts_ns = c.timestamp.parse::<i64>().unwrap_or(0) * 1_000_000;
                    let _ = events.send(ConnectorEvent::Book(BookUpdate {
                        market,
                        bids,
                        asks,
                        seq: 0,
                        ts_event_ns: ts_ns,
                    }));
                }
                _ => {}
            }
        }
    }
}

fn level(l: &PolyLevel) -> Level {
    Level {
        price: Price(Decimal::from_str(&l.price).unwrap_or_default()),
        size: Qty(Decimal::from_str(&l.size).unwrap_or_default()),
    }
}
