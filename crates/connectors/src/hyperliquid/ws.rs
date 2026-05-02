use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use hl_omm_core::{
    BookUpdate, ClientOrderId, FillEvent, Level, MarketKey, OrderState, Price, Qty, Side, Venue,
};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use super::types::{
    L2BookPayload, OrderUpdatePayload, RawLevel, TradePayload, UserEventsPayload, UserFill,
    WsRequest, WsResponse, WsSubscription,
};
use crate::common::{ConnectorCommand, ConnectorEvent};

/// Shape of a single WS connection. Owns one tokio task for read, one for
/// the heartbeat. Reconnects with exponential backoff on disconnect.
pub struct HyperliquidWs {
    ws_url: String,
    user: Option<String>,
    venue_for_outcome: bool,
}

impl HyperliquidWs {
    pub fn new(ws_url: impl Into<String>, user: Option<String>, venue_for_outcome: bool) -> Self {
        Self {
            ws_url: ws_url.into(),
            user,
            venue_for_outcome,
        }
    }

    /// Spawn the connection. The returned `events` receiver yields normalised
    /// `ConnectorEvent`s; the `commands` sender accepts subscribe/unsubscribe
    /// requests (orders are routed through the REST client, not WS).
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
            match self.connect_loop(&events, &mut commands).await {
                Ok(()) => break,
                Err(e) => {
                    warn!(error = %e, "hyperliquid ws disconnected");
                    let venue = if self.venue_for_outcome {
                        Venue::HyperliquidOutcome
                    } else {
                        Venue::HyperliquidPerp
                    };
                    let _ = events.send(ConnectorEvent::Resyncing { venue });
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(15));
                }
            }
        }
    }

    async fn connect_loop(
        &self,
        events: &mpsc::UnboundedSender<ConnectorEvent>,
        commands: &mut mpsc::UnboundedReceiver<ConnectorCommand>,
    ) -> Result<()> {
        let (ws, _) = connect_async(&self.ws_url).await.context("ws connect")?;
        info!(url = %self.ws_url, "hyperliquid ws connected");
        let (mut sink, mut stream) = ws.split();

        // user-scoped subscriptions auto-resubscribe on each reconnect.
        if let Some(user) = &self.user {
            let req = WsRequest {
                method: "subscribe".into(),
                subscription: WsSubscription::UserEvents { user: user.clone() },
            };
            sink.send(Message::Text(serde_json::to_string(&req)?)).await?;
            let req = WsRequest {
                method: "subscribe".into(),
                subscription: WsSubscription::OrderUpdates { user: user.clone() },
            };
            sink.send(Message::Text(serde_json::to_string(&req)?)).await?;
        }

        let venue = if self.venue_for_outcome {
            Venue::HyperliquidOutcome
        } else {
            Venue::HyperliquidPerp
        };
        let _ = events.send(ConnectorEvent::Resynced { venue });

        let mut hb = tokio::time::interval(Duration::from_secs(20));
        hb.tick().await;

        loop {
            tokio::select! {
                msg = stream.next() => {
                    let Some(msg) = msg else { break };
                    let msg = msg.context("ws read")?;
                    match msg {
                        Message::Text(t) => self.handle_text(&t, events)?,
                        Message::Binary(b) => self.handle_text(&String::from_utf8_lossy(&b), events)?,
                        Message::Ping(p) => { sink.send(Message::Pong(p)).await?; }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
                cmd = commands.recv() => {
                    let Some(cmd) = cmd else { return Ok(()) };
                    match cmd {
                        ConnectorCommand::Subscribe(market) => {
                            let req = WsRequest {
                                method: "subscribe".into(),
                                subscription: WsSubscription::L2Book {
                                    coin: market.instrument.0.clone(),
                                    n_sig_figs: Some(5),
                                    n_levels: Some(20),
                                },
                            };
                            sink.send(Message::Text(serde_json::to_string(&req)?)).await?;
                            let req = WsRequest {
                                method: "subscribe".into(),
                                subscription: WsSubscription::Trades {
                                    coin: market.instrument.0.clone(),
                                },
                            };
                            sink.send(Message::Text(serde_json::to_string(&req)?)).await?;
                        }
                        ConnectorCommand::Unsubscribe(market) => {
                            let req = WsRequest {
                                method: "unsubscribe".into(),
                                subscription: WsSubscription::L2Book {
                                    coin: market.instrument.0.clone(),
                                    n_sig_figs: None,
                                    n_levels: None,
                                },
                            };
                            sink.send(Message::Text(serde_json::to_string(&req)?)).await?;
                        }
                        // Orders / cancels go through REST.
                        _ => {}
                    }
                }
                _ = hb.tick() => {
                    sink.send(Message::Text("{\"method\":\"ping\"}".into())).await?;
                }
            }
        }
        Ok(())
    }

    fn handle_text(&self, text: &str, events: &mpsc::UnboundedSender<ConnectorEvent>) -> Result<()> {
        let resp: WsResponse = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                debug!(error = %e, msg = %text, "ws decode failed");
                return Ok(());
            }
        };
        match resp {
            WsResponse::L2Book(book) => {
                let venue = self.venue_for_coin(&book.coin);
                let market = MarketKey::new(venue, book.coin.clone());
                let bids = book.levels[0].iter().map(level_from).collect();
                let asks = book.levels[1].iter().map(level_from).collect();
                let _ = events.send(ConnectorEvent::Book(BookUpdate {
                    market,
                    bids,
                    asks,
                    seq: 0,
                    ts_event_ns: book.time * 1_000_000,
                }));
            }
            WsResponse::Trades(trades) => {
                for t in trades {
                    push_trade(events, &t, self.venue_for_coin(&t.coin));
                }
            }
            WsResponse::UserEvents(UserEventsPayload { fills }) => {
                for fill in fills {
                    push_fill(events, &fill, self.venue_for_coin(&fill.coin));
                }
            }
            WsResponse::OrderUpdates(updates) => {
                for upd in updates {
                    push_order_update(events, &upd, self.venue_for_coin(&upd.order.coin));
                }
            }
            WsResponse::Bbo(_) | WsResponse::Pong | WsResponse::SubscriptionResponse(_) => {}
            WsResponse::Error(e) => warn!(?e, "hyperliquid ws error"),
            WsResponse::Unknown => {}
        }
        Ok(())
    }

    fn venue_for_coin(&self, coin: &str) -> Venue {
        if coin.starts_with("OUT:") {
            Venue::HyperliquidOutcome
        } else if coin.contains('/') {
            Venue::HyperliquidSpot
        } else {
            Venue::HyperliquidPerp
        }
    }
}

fn level_from(r: &RawLevel) -> Level {
    Level {
        price: Price(Decimal::from_str(&r.px).unwrap_or_default()),
        size: Qty(Decimal::from_str(&r.sz).unwrap_or_default()),
    }
}

fn push_trade(events: &mpsc::UnboundedSender<ConnectorEvent>, t: &TradePayload, venue: Venue) {
    let market = MarketKey::new(venue, t.coin.clone());
    let side = if t.side == "B" { Side::Buy } else { Side::Sell };
    let _ = events.send(ConnectorEvent::Trade {
        market,
        price: Price(Decimal::from_str(&t.px).unwrap_or_default()),
        qty: Qty(Decimal::from_str(&t.sz).unwrap_or_default()),
        side,
        ts_event_ns: t.time * 1_000_000,
    });
}

fn push_fill(events: &mpsc::UnboundedSender<ConnectorEvent>, fill: &UserFill, venue: Venue) {
    let market = MarketKey::new(venue, fill.coin.clone());
    let side = if fill.side == "B" { Side::Buy } else { Side::Sell };
    let cloid = fill
        .cloid
        .as_deref()
        .and_then(|s| u128::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .map(ClientOrderId::new)
        .unwrap_or(ClientOrderId(0));
    let event = FillEvent {
        cloid,
        market,
        side,
        price: Price(Decimal::from_str(&fill.px).unwrap_or_default()),
        qty: Qty(Decimal::from_str(&fill.sz).unwrap_or_default()),
        fee: Decimal::from_str(&fill.fee).unwrap_or_default(),
        ts_ns: fill.time * 1_000_000,
        is_maker: fill.liquidity.as_deref() == Some("M"),
    };
    let _ = events.send(ConnectorEvent::Fill(event));
}

fn push_order_update(
    events: &mpsc::UnboundedSender<ConnectorEvent>,
    upd: &OrderUpdatePayload,
    venue: Venue,
) {
    let cloid = upd
        .order
        .cloid
        .as_deref()
        .and_then(|s| u128::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .map(ClientOrderId::new);
    if cloid.is_none() {
        return;
    }
    let state = match upd.status.as_str() {
        "open" => OrderState::Open,
        "filled" => OrderState::Filled,
        "canceled" | "marginCanceled" => OrderState::Canceled,
        "triggered" | "rejected" => OrderState::Rejected,
        _ => OrderState::Open,
    };
    let _ = events.send(ConnectorEvent::OrderUpdate {
        cloid: cloid.unwrap(),
        venue,
        state,
        venue_oid: Some(upd.order.oid.to_string()),
    });
}
