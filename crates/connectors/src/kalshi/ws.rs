use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use hl_omm_core::{BookUpdate, Level, MarketKey, Price, Qty, Venue};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::common::{ConnectorCommand, ConnectorEvent};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum KalshiEvent {
    OrderbookSnapshot(OrderbookSnapshot),
    OrderbookDelta(OrderbookDelta),
    Ticker(serde_json::Value),
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct OrderbookSnapshot {
    msg: SnapshotMsg,
}

#[derive(Debug, Deserialize)]
struct SnapshotMsg {
    market_ticker: String,
    yes: Vec<[String; 2]>, // [price_cents, size]
    no: Vec<[String; 2]>,
}

#[derive(Debug, Deserialize)]
struct OrderbookDelta {
    msg: DeltaMsg,
}

#[derive(Debug, Deserialize)]
struct DeltaMsg {
    market_ticker: String,
    side: String, // "yes" | "no"
    price: i64,
    delta: i64,
}

pub struct KalshiWs {
    auth_token: String,
}

impl KalshiWs {
    pub fn new(auth_token: String) -> Self {
        Self { auth_token }
    }

    pub fn spawn(
        self,
    ) -> (
        mpsc::UnboundedReceiver<ConnectorEvent>,
        mpsc::UnboundedSender<ConnectorCommand>,
    ) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(self.run(event_tx, cmd_rx));
        (event_rx, cmd_tx)
    }

    async fn run(
        self,
        events: mpsc::UnboundedSender<ConnectorEvent>,
        mut commands: mpsc::UnboundedReceiver<ConnectorCommand>,
    ) {
        let mut backoff = Duration::from_millis(250);
        loop {
            match self.session(&events, &mut commands).await {
                Ok(()) => break,
                Err(e) => {
                    warn!(error = %e, "kalshi ws disconnected");
                    let _ = events.send(ConnectorEvent::Resyncing { venue: Venue::Kalshi });
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(15));
                }
            }
        }
    }

    async fn session(
        &self,
        events: &mpsc::UnboundedSender<ConnectorEvent>,
        commands: &mut mpsc::UnboundedReceiver<ConnectorCommand>,
    ) -> Result<()> {
        let url = format!("{}", super::WS_URL);
        let (ws, _) = connect_async(&url).await.context("kalshi ws connect")?;
        info!(url = %url, "kalshi ws connected");
        let (mut sink, mut stream) = ws.split();

        // Auth message — Kalshi uses a JWT-style session token.
        let auth = serde_json::json!({
            "id": 1,
            "cmd": "auth",
            "params": {"token": self.auth_token},
        });
        sink.send(Message::Text(auth.to_string())).await?;
        let _ = events.send(ConnectorEvent::Resynced { venue: Venue::Kalshi });

        let mut hb = tokio::time::interval(Duration::from_secs(15));
        hb.tick().await;
        let mut next_id = 2u64;

        loop {
            tokio::select! {
                msg = stream.next() => {
                    let Some(msg) = msg else { break };
                    let msg = msg.context("kalshi ws read")?;
                    if let Message::Text(t) = msg {
                        Self::handle(&t, events);
                    }
                }
                cmd = commands.recv() => {
                    let Some(cmd) = cmd else { return Ok(()) };
                    if let ConnectorCommand::Subscribe(market) = cmd {
                        let body = serde_json::json!({
                            "id": next_id,
                            "cmd": "subscribe",
                            "params": {
                                "channels": ["orderbook_delta", "ticker"],
                                "market_tickers": [market.instrument.0],
                            }
                        });
                        next_id += 1;
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
        let ev: KalshiEvent = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return,
        };
        match ev {
            KalshiEvent::OrderbookSnapshot(snap) => {
                let market = MarketKey::new(Venue::Kalshi, snap.msg.market_ticker);
                let bids = snap
                    .msg
                    .yes
                    .iter()
                    .map(|[p, s]| Level {
                        price: Price(Decimal::from_str(p).unwrap_or_default() / dec!(100)),
                        size: Qty(Decimal::from_str(s).unwrap_or_default()),
                    })
                    .collect();
                // Kalshi reports YES bids and NO bids; the NO side maps to
                // (1 - price) for the YES leg, which gives us a symmetric ask
                // ladder. We invert here so the strategy layer sees a normal
                // bid/ask book on the YES asset.
                let asks = snap
                    .msg
                    .no
                    .iter()
                    .map(|[p, s]| Level {
                        price: Price(dec!(1) - Decimal::from_str(p).unwrap_or_default() / dec!(100)),
                        size: Qty(Decimal::from_str(s).unwrap_or_default()),
                    })
                    .collect();
                let _ = events.send(ConnectorEvent::Book(BookUpdate {
                    market,
                    bids,
                    asks,
                    seq: 0,
                    ts_event_ns: 0,
                }));
            }
            KalshiEvent::OrderbookDelta(_) | KalshiEvent::Ticker(_) | KalshiEvent::Other => {}
        }
    }
}
