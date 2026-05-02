//! Performance dashboard. Serves a small static webpage (Plotly.js) and a
//! WebSocket stream of bot state (positions, PnL, latency, signals).
//!
//! The dashboard is intentionally read-only — it never gets to fire orders.
use anyhow::Result;
use arc_swap::ArcSwap;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use hl_omm_core::MarketKey;
use hl_omm_risk::{RiskBook, RiskSnapshot};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tower_http::services::ServeDir;
use tracing::{info, warn};

#[derive(Clone)]
pub struct DashboardState {
    pub risk: Arc<RiskBook>,
    pub history: Arc<RwLock<VecDeque<PnlPoint>>>,
    pub signals: Arc<RwLock<VecDeque<SignalPoint>>>,
    pub last_books: Arc<ArcSwap<Vec<BookSummary>>>,
    pub latency: Arc<RwLock<LatencyMeter>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PnlPoint {
    pub ts_ms: i64,
    pub pnl: f64,
    pub strategy: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignalPoint {
    pub ts_ms: i64,
    pub strategy: String,
    pub kind: String,
    pub edge: f64,
    pub market: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BookSummary {
    pub market: MarketKey,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub bid_size: Option<f64>,
    pub ask_size: Option<f64>,
    pub microprice: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LatencyMeter {
    pub md_p50_us: f64,
    pub md_p99_us: f64,
    pub order_rtt_p50_us: f64,
    pub order_rtt_p99_us: f64,
    pub samples: usize,
}

#[derive(Serialize)]
struct Snapshot<'a> {
    ts_ms: i64,
    risk: &'a RiskSnapshot,
    history: &'a [PnlPoint],
    signals: &'a [SignalPoint],
    books: &'a [BookSummary],
    latency: &'a LatencyMeter,
}

impl DashboardState {
    pub fn new(risk: Arc<RiskBook>) -> Self {
        Self {
            risk,
            history: Arc::new(RwLock::new(VecDeque::with_capacity(4096))),
            signals: Arc::new(RwLock::new(VecDeque::with_capacity(2048))),
            last_books: Arc::new(ArcSwap::from_pointee(Vec::new())),
            latency: Arc::new(RwLock::new(LatencyMeter::default())),
        }
    }

    pub fn push_pnl(&self, p: PnlPoint) {
        let mut h = self.history.write();
        h.push_back(p);
        while h.len() > 4096 {
            h.pop_front();
        }
    }

    pub fn push_signal(&self, s: SignalPoint) {
        let mut h = self.signals.write();
        h.push_back(s);
        while h.len() > 2048 {
            h.pop_front();
        }
    }

    pub fn set_books(&self, books: Vec<BookSummary>) {
        self.last_books.store(Arc::new(books));
    }
}

pub async fn serve(addr: SocketAddr, state: DashboardState, static_dir: String) -> Result<()> {
    let app = Router::new()
        .route("/api/snapshot", get(snapshot))
        .route("/api/stream", get(ws_handler))
        .nest_service("/", ServeDir::new(static_dir))
        .with_state(state.clone());

    info!(%addr, "dashboard listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn snapshot(State(state): State<DashboardState>) -> impl IntoResponse {
    let risk = state.risk.snapshot();
    let history = state.history.read().iter().cloned().collect::<Vec<_>>();
    let signals = state.signals.read().iter().cloned().collect::<Vec<_>>();
    let books = state.last_books.load_full();
    let latency = state.latency.read().clone();
    let payload = Snapshot {
        ts_ms: chrono::Utc::now().timestamp_millis(),
        risk: &risk,
        history: &history,
        signals: &signals,
        books: books.as_ref(),
        latency: &latency,
    };
    axum::Json(serde_json::to_value(&payload).unwrap_or_default())
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<DashboardState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_loop(socket, state))
}

async fn ws_loop(mut socket: WebSocket, state: DashboardState) {
    let mut tick = interval(Duration::from_millis(250));
    loop {
        tick.tick().await;
        let risk = state.risk.snapshot();
        let history = state.history.read().iter().cloned().collect::<Vec<_>>();
        let signals = state.signals.read().iter().cloned().collect::<Vec<_>>();
        let books = state.last_books.load_full();
        let latency = state.latency.read().clone();
        let payload = Snapshot {
            ts_ms: chrono::Utc::now().timestamp_millis(),
            risk: &risk,
            history: &history,
            signals: &signals,
            books: books.as_ref(),
            latency: &latency,
        };
        let json = match serde_json::to_string(&payload) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "snapshot encode failed");
                continue;
            }
        };
        if socket.send(Message::Text(json)).await.is_err() {
            break;
        }
    }
}
