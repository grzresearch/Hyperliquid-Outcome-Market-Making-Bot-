use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use hl_omm_connectors::common::{ConnectorCommand, ConnectorEvent};
use hl_omm_connectors::hyperliquid::{
    outcome::parse_outcome_market_id, HyperliquidClient, REST_URL, TESTNET_REST_URL,
    TESTNET_WS_URL, WS_URL,
};
use hl_omm_connectors::hyperliquid::signing::ApiCreds;
use hl_omm_connectors::hyperliquid::ws::HyperliquidWs;
use hl_omm_connectors::polymarket::client::{PolyApiCreds, PolymarketClient};
use hl_omm_connectors::polymarket::ws::PolymarketWs;
use hl_omm_connectors::kalshi::client::{KalshiClient, KalshiCreds};
use hl_omm_connectors::kalshi::ws::KalshiWs;
use hl_omm_core::{BookUpdate, MarketKey, Order, OrderBook, OutcomeKey, ThresholdDirection, Venue};
use hl_omm_dashboard::{BookSummary, DashboardState, PnlPoint, SignalPoint};
use hl_omm_risk::{RiskBook, RiskDecision, RiskLimits};
use hl_omm_strategies::{
    avellaneda::{AvellanedaParams, AvellanedaStoikov},
    btc_parity::{BtcParity, ParityParams},
    common::{StrategyContext, StrategyEvent},
    xvenue_arb::{ArbParams, CrossVenueArb, VenueLeg},
};
use parking_lot::RwLock;
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

mod config_io;

#[derive(Debug, Deserialize, Clone)]
struct AppConfig {
    network: NetworkCfg,
    risk: RiskLimits,
    strategy: StrategyCfg,
    dashboard: DashboardCfg,
    venues: VenuesCfg,
}

#[derive(Debug, Deserialize, Clone)]
struct NetworkCfg {
    is_mainnet: bool,
}

#[derive(Debug, Deserialize, Clone)]
struct StrategyCfg {
    avellaneda: AvellanedaParams,
    parity: ParityParams,
    arb: ArbParams,
    btc_strike_usd: rust_decimal::Decimal,
    btc_expiry_ts_ns: i64,
    market_yes: String,
    market_no: String,
    btc_perp_coin: String,
    polymarket_yes_id: Option<String>,
    polymarket_no_id: Option<String>,
    kalshi_yes_ticker: Option<String>,
    kalshi_no_ticker: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct DashboardCfg {
    bind: String,
    static_dir: String,
}

#[derive(Debug, Deserialize, Clone)]
struct VenuesCfg {
    hyperliquid_pk: Option<String>,
    hyperliquid_vault: Option<String>,
    polymarket_key: Option<String>,
    polymarket_secret: Option<String>,
    polymarket_passphrase: Option<String>,
    polymarket_maker: Option<String>,
    kalshi_key_id: Option<String>,
    kalshi_pem_path: Option<String>,
    kalshi_ws_token: Option<String>,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("hl_omm=debug,info")),
        )
        .with_target(true)
        .json()
        .init();

    let cfg = config_io::load()?;
    info!("starting Hyperliquid Outcome MM bot");

    let risk = Arc::new(RiskBook::new(cfg.risk.clone()));
    let dash = DashboardState::new(risk.clone());

    let dashboard_state = dash.clone();
    let dash_addr: SocketAddr = cfg.dashboard.bind.parse()?;
    let dash_static = cfg.dashboard.static_dir.clone();
    tokio::spawn(async move {
        if let Err(e) = hl_omm_dashboard::serve(dash_addr, dashboard_state, dash_static).await {
            error!(error = %e, "dashboard server crashed");
        }
    });

    // ---- Hyperliquid REST client (perp + spot + outcome share one client).
    let hl_creds = ApiCreds::from_private_key(
        cfg.venues
            .hyperliquid_pk
            .as_deref()
            .context("hyperliquid_pk missing")?,
        cfg.venues.hyperliquid_vault.clone(),
    )?;
    let rest = if cfg.network.is_mainnet { REST_URL } else { TESTNET_REST_URL };
    let ws_url = if cfg.network.is_mainnet { WS_URL } else { TESTNET_WS_URL };
    let hl = HyperliquidClient::new(rest.to_string(), hl_creds.clone(), cfg.network.is_mainnet);
    hl.refresh_meta().await.ok();
    let address = hl.address();

    // ---- Hyperliquid WS (one connection serves perp + outcome).
    let hl_ws = HyperliquidWs::new(ws_url.to_string(), Some(address.clone()), true);
    let (mut hl_events, hl_cmds) = hl_ws.spawn();

    // ---- Polymarket.
    let _polymarket = match (
        cfg.venues.polymarket_key.as_deref(),
        cfg.venues.polymarket_secret.as_deref(),
        cfg.venues.polymarket_passphrase.as_deref(),
        cfg.venues.polymarket_maker.as_deref(),
    ) {
        (Some(k), Some(s), Some(p), Some(m)) => Some(PolymarketClient::new(PolyApiCreds {
            key: k.into(), secret: s.into(), passphrase: p.into(), maker: m.into(),
        })),
        _ => None,
    };
    let (mut poly_events, poly_cmds) = PolymarketWs::spawn();

    // ---- Kalshi.
    let _kalshi = match (
        cfg.venues.kalshi_key_id.as_deref(),
        cfg.venues.kalshi_pem_path.as_deref(),
    ) {
        (Some(id), Some(path)) => {
            let pem = tokio::fs::read_to_string(path).await?;
            Some(KalshiClient::new(KalshiCreds::from_pem(id.into(), &pem)?))
        }
        _ => None,
    };
    let (mut kalshi_events, kalshi_cmds) = KalshiWs::new(
        cfg.venues
            .kalshi_ws_token
            .clone()
            .unwrap_or_default(),
    )
    .spawn();

    // ---- Wire commands by venue.
    let mut commands: HashMap<Venue, mpsc::UnboundedSender<ConnectorCommand>> = HashMap::new();
    commands.insert(Venue::HyperliquidOutcome, hl_cmds.clone());
    commands.insert(Venue::HyperliquidPerp, hl_cmds.clone());
    commands.insert(Venue::HyperliquidSpot, hl_cmds.clone());
    commands.insert(Venue::Polymarket, poly_cmds.clone());
    commands.insert(Venue::Kalshi, kalshi_cmds.clone());

    let ctx = Arc::new(StrategyContext::new(commands.clone()));

    // ---- Subscribe to all the markets we care about.
    let yes_market = MarketKey::new(Venue::HyperliquidOutcome, cfg.strategy.market_yes.clone());
    let no_market = MarketKey::new(Venue::HyperliquidOutcome, cfg.strategy.market_no.clone());
    let perp_market = MarketKey::new(Venue::HyperliquidPerp, cfg.strategy.btc_perp_coin.clone());
    hl_cmds.send(ConnectorCommand::Subscribe(yes_market.clone()))?;
    hl_cmds.send(ConnectorCommand::Subscribe(no_market.clone()))?;
    hl_cmds.send(ConnectorCommand::Subscribe(perp_market.clone()))?;

    // Build the cross-venue legs that exist.
    let mut legs: Vec<VenueLeg> = vec![VenueLeg {
        venue: Venue::HyperliquidOutcome,
        yes_market: yes_market.clone(),
        no_market: no_market.clone(),
    }];
    if let (Some(yid), Some(nid)) = (cfg.strategy.polymarket_yes_id.clone(), cfg.strategy.polymarket_no_id.clone()) {
        let y = MarketKey::new(Venue::Polymarket, yid);
        let n = MarketKey::new(Venue::Polymarket, nid);
        poly_cmds.send(ConnectorCommand::Subscribe(y.clone()))?;
        poly_cmds.send(ConnectorCommand::Subscribe(n.clone()))?;
        legs.push(VenueLeg { venue: Venue::Polymarket, yes_market: y, no_market: n });
    }
    if let (Some(yt), Some(nt)) = (cfg.strategy.kalshi_yes_ticker.clone(), cfg.strategy.kalshi_no_ticker.clone()) {
        let y = MarketKey::new(Venue::Kalshi, yt);
        let n = MarketKey::new(Venue::Kalshi, nt);
        kalshi_cmds.send(ConnectorCommand::Subscribe(y.clone()))?;
        kalshi_cmds.send(ConnectorCommand::Subscribe(n.clone()))?;
        legs.push(VenueLeg { venue: Venue::Kalshi, yes_market: y, no_market: n });
    }

    // ---- Build event key from the YES ticker.
    let outcome_event = match parse_outcome_market_id(&cfg.strategy.market_yes) {
        Some(t) => OutcomeKey {
            underlying: t.underlying.clone(),
            strike_cents: (t.strike * rust_decimal::Decimal::from(100))
                .trunc()
                .to_string()
                .parse()
                .unwrap_or(0),
            expiry_ns: cfg.strategy.btc_expiry_ts_ns,
            direction: ThresholdDirection::Above,
        },
        None => OutcomeKey {
            underlying: "BTC".into(),
            strike_cents: 0,
            expiry_ns: cfg.strategy.btc_expiry_ts_ns,
            direction: ThresholdDirection::Above,
        },
    };

    // ---- Strategies.
    let (strat_tx, mut strat_rx) = mpsc::unbounded_channel::<StrategyEvent>();
    let avs = Arc::new(AvellanedaStoikov::new(
        cfg.strategy.avellaneda.clone(),
        yes_market.clone(),
        no_market.clone(),
        cfg.strategy.btc_expiry_ts_ns,
        ctx.clone(),
        strat_tx.clone(),
    ));
    let parity = Arc::new(BtcParity::new(
        cfg.strategy.parity.clone(),
        yes_market.clone(),
        no_market.clone(),
        perp_market.clone(),
        cfg.strategy.btc_strike_usd,
        cfg.strategy.btc_expiry_ts_ns,
        ctx.clone(),
        strat_tx.clone(),
    ));
    let xarb = Arc::new(CrossVenueArb::new(
        cfg.strategy.arb.clone(),
        outcome_event,
        legs,
        ctx.clone(),
        strat_tx.clone(),
    ));

    // ---- Event pump. One task drains each connector, normalises to
    // `ConnectorEvent`, fans out to strategies + the dashboard.
    let dash_for_pump = dash.clone();
    let ctx_for_pump = ctx.clone();
    let avs_for_pump = avs.clone();
    let parity_for_pump = parity.clone();
    let xarb_for_pump = xarb.clone();
    let risk_for_pump = risk.clone();
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                Some(ev) = hl_events.recv() => ev,
                Some(ev) = poly_events.recv() => ev,
                Some(ev) = kalshi_events.recv() => ev,
                else => break,
            };
            handle_event(
                event,
                &dash_for_pump,
                &ctx_for_pump,
                avs_for_pump.clone(),
                parity_for_pump.clone(),
                xarb_for_pump.clone(),
                &risk_for_pump,
            );
        }
    });

    // ---- Strategy event router. Risk gate -> connector.
    let dash_for_router = dash.clone();
    let risk_for_router = risk.clone();
    let hl_for_orders = Arc::new(hl);
    tokio::spawn(async move {
        while let Some(ev) = strat_rx.recv().await {
            match ev {
                StrategyEvent::Quote(q) | StrategyEvent::Take(q) => {
                    let order = Order::new_limit(
                        q.cloid,
                        q.market.clone(),
                        q.side,
                        q.price,
                        q.qty,
                        q.post_only,
                        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0),
                    );
                    let decision = risk_for_router.check(&order);
                    let order = match decision {
                        RiskDecision::Pass => order,
                        RiskDecision::Resize(qty) => {
                            let mut o = order.clone();
                            o.qty = hl_omm_core::Qty(qty);
                            o
                        }
                        RiskDecision::Reject(why) => {
                            warn!(?why, ?q.market, "risk rejected");
                            continue;
                        }
                    };

                    dash_for_router.push_signal(SignalPoint {
                        ts_ms: chrono::Utc::now().timestamp_millis(),
                        strategy: q.strategy.as_str().into(),
                        kind: format!("{:?}", q.side),
                        edge: q.price.to_f64(),
                        market: q.market.to_string(),
                    });

                    match q.market.venue {
                        Venue::HyperliquidOutcome | Venue::HyperliquidPerp | Venue::HyperliquidSpot => {
                            let hl = hl_for_orders.clone();
                            let market = q.market.clone();
                            let risk = risk_for_router.clone();
                            tokio::spawn(async move {
                                match hl.place_order(&order).await {
                                    Ok(_) => risk.on_order_acked(&market),
                                    Err(e) => warn!(error = %e, "place_order failed"),
                                }
                            });
                        }
                        _ => {
                            // Polymarket / Kalshi orders go through their own
                            // REST clients (omitted: see crates/connectors).
                        }
                    }
                }
                StrategyEvent::Cancel { market, cloid } => {
                    let hl = hl_for_orders.clone();
                    tokio::spawn(async move {
                        if let Err(e) = hl.cancel_by_cloid(&market, cloid).await {
                            warn!(error = %e, "cancel failed");
                        }
                    });
                }
            }
        }
    });

    // ---- PnL sampler.
    {
        let dash = dash.clone();
        let risk = risk.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
            loop {
                tick.tick().await;
                let pnl = risk.pnl();
                use rust_decimal::prelude::ToPrimitive;
                dash.push_pnl(PnlPoint {
                    ts_ms: chrono::Utc::now().timestamp_millis(),
                    pnl: pnl.to_f64().unwrap_or(0.0),
                    strategy: "total".into(),
                });
            }
        });
    }

    info!("bot is up; ctrl-c to exit");
    tokio::signal::ctrl_c().await.ok();
    Ok(())
}

fn handle_event(
    event: ConnectorEvent,
    dash: &DashboardState,
    ctx: &Arc<StrategyContext>,
    avs: Arc<AvellanedaStoikov>,
    parity: Arc<BtcParity>,
    xarb: Arc<CrossVenueArb>,
    risk: &Arc<RiskBook>,
) {
    let now_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    match event {
        ConnectorEvent::Book(b) => {
            update_book(&ctx.books, b.clone(), now_ns);
            update_dashboard_books(dash, ctx);
            avs.on_book(&book_for(&ctx.books, &b.market).unwrap(), now_ns);
            parity.on_tick(now_ns);
            xarb.on_tick();
        }
        ConnectorEvent::Trade { .. } => {}
        ConnectorEvent::Fill(fill) => {
            risk.on_fill(&fill);
            risk.on_order_done(&fill.market);
        }
        ConnectorEvent::OrderUpdate { .. } => {}
        ConnectorEvent::Resyncing { venue } => warn!(?venue, "resyncing"),
        ConnectorEvent::Resynced { venue } => info!(?venue, "resynced"),
    }
}

fn update_book(
    books: &Arc<RwLock<HashMap<MarketKey, OrderBook>>>,
    update: BookUpdate,
    now_ns: i64,
) {
    let mut g = books.write();
    let entry = g.entry(update.market.clone()).or_insert_with(|| OrderBook::empty(update.market.clone()));
    entry.bids = update.bids;
    entry.asks = update.asks;
    entry.seq = update.seq;
    entry.ts_event_ns = update.ts_event_ns;
    entry.ts_recv_ns = now_ns;
}

fn book_for(books: &Arc<RwLock<HashMap<MarketKey, OrderBook>>>, market: &MarketKey) -> Option<OrderBook> {
    books.read().get(market).cloned()
}

fn update_dashboard_books(dash: &DashboardState, ctx: &Arc<StrategyContext>) {
    let snap: Vec<BookSummary> = ctx
        .books
        .read()
        .iter()
        .map(|(k, b)| BookSummary {
            market: k.clone(),
            bid: b.best_bid().map(|l| l.price.to_f64()),
            ask: b.best_ask().map(|l| l.price.to_f64()),
            bid_size: b.best_bid().map(|l| l.size.to_f64()),
            ask_size: b.best_ask().map(|l| l.size.to_f64()),
            microprice: b.microprice().map(|p| p.to_f64()),
        })
        .collect();
    dash.set_books(snap);
}
