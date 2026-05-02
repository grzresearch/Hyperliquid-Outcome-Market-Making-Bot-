use anyhow::{Context, Result};
use dashmap::DashMap;
use hl_omm_core::{ClientOrderId, MarketKey, Order, Side, TimeInForce, Venue};
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, instrument, warn};

use super::signing::{sign_action, ApiCreds};
use super::types::{
    CancelByCloidAction, CancelByCloidRequest, ExchangeAction, LimitTif, OrderAction,
    OrderRequest, OrderTrigger, OutcomeMeta, PerpMeta, SignatureRsV, SignedExchangeRequest,
};

/// Client for the Hyperliquid REST surface — `/info` and `/exchange`.
///
/// One instance fronts perp, spot and outcome markets. Subscriptions /
/// streaming live in the [`super::ws`] module; this struct is just the
/// signed write path.
#[derive(Clone)]
pub struct HyperliquidClient {
    rest_url: String,
    is_mainnet: bool,
    creds: Arc<ApiCreds>,
    http: Client,
    /// asset_index lookup keyed by ticker. Populated lazily from `meta` /
    /// `outcomeMeta`. Stored in a DashMap because both the strategy thread and
    /// reconnect loop need to read it simultaneously.
    asset_idx: Arc<DashMap<String, u32>>,
}

impl HyperliquidClient {
    pub fn new(rest_url: impl Into<String>, creds: ApiCreds, is_mainnet: bool) -> Self {
        let http = Client::builder()
            .pool_idle_timeout(Some(Duration::from_secs(30)))
            .timeout(Duration::from_secs(5))
            .tcp_nodelay(true)
            .tcp_keepalive(Some(Duration::from_secs(15)))
            .build()
            .expect("reqwest builder");
        Self {
            rest_url: rest_url.into(),
            is_mainnet,
            creds: Arc::new(creds),
            http,
            asset_idx: Arc::new(DashMap::new()),
        }
    }

    pub fn venue_for(&self, market: &MarketKey) -> Venue {
        market.venue
    }

    pub async fn refresh_meta(&self) -> Result<()> {
        let perp_meta: PerpMeta = self
            .http
            .post(format!("{}/info", self.rest_url))
            .json(&serde_json::json!({"type": "meta"}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        for (idx, asset) in perp_meta.universe.iter().enumerate() {
            self.asset_idx.insert(asset.name.clone(), idx as u32);
        }

        // outcomeMeta is the new HIP-4 endpoint that returns the YES/NO
        // pair listing. The asset index space is shared with perps + spot
        // (perps occupy 0..N, spot occupies 10000..10000+M, outcome 20000..).
        let outcome_meta: OutcomeMeta = self
            .http
            .post(format!("{}/info", self.rest_url))
            .json(&serde_json::json!({"type": "outcomeMeta"}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .unwrap_or(OutcomeMeta { markets: vec![] });

        for (i, m) in outcome_meta.markets.iter().enumerate() {
            self.asset_idx
                .insert(m.yes_coin.clone(), 20_000 + (i as u32) * 2);
            self.asset_idx
                .insert(m.no_coin.clone(), 20_000 + (i as u32) * 2 + 1);
        }
        debug!(perp = perp_meta.universe.len(), outcome = outcome_meta.markets.len(), "meta refreshed");
        Ok(())
    }

    /// Resolve a venue ticker (e.g. "BTC", "OUT:BTC-78213-2026-05-03-YES") to
    /// its asset index. Falls back to a refresh if the cache misses.
    pub async fn asset_index(&self, ticker: &str) -> Result<u32> {
        if let Some(idx) = self.asset_idx.get(ticker) {
            return Ok(*idx);
        }
        self.refresh_meta().await?;
        self.asset_idx
            .get(ticker)
            .map(|x| *x)
            .with_context(|| format!("unknown ticker after refresh: {ticker}"))
    }

    #[instrument(level = "debug", skip(self))]
    pub async fn place_order(&self, order: &Order) -> Result<u64> {
        let asset = self.asset_index(&order.market.instrument.0).await?;
        let tif = match order.tif {
            TimeInForce::Gtc => "Gtc",
            TimeInForce::Ioc => "Ioc",
            TimeInForce::Alo => "Alo",
        };
        let req = OrderRequest {
            a: asset,
            b: matches!(order.side, Side::Buy),
            p: order.price.0.normalize().to_string(),
            s: order.qty.0.normalize().to_string(),
            r: false,
            t: OrderTrigger {
                limit: LimitTif { tif: tif.into() },
            },
            c: Some(order.cloid.to_hex()),
        };
        let action = ExchangeAction::Order(OrderAction {
            orders: vec![req],
            grouping: "na".into(),
        });
        let resp = self.send(&action).await?;
        let oid = resp
            .pointer("/response/data/statuses/0/resting/oid")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                resp.pointer("/response/data/statuses/0/filled/oid")
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(0);
        Ok(oid)
    }

    #[instrument(level = "debug", skip(self))]
    pub async fn cancel_by_cloid(&self, market: &MarketKey, cloid: ClientOrderId) -> Result<()> {
        let asset = self.asset_index(&market.instrument.0).await?;
        let action = ExchangeAction::CancelByCloid(CancelByCloidAction {
            cancels: vec![CancelByCloidRequest {
                asset,
                cloid: cloid.to_hex(),
            }],
        });
        let resp = self.send(&action).await?;
        if resp
            .pointer("/response/data/statuses/0")
            .and_then(|v| v.as_str())
            != Some("success")
        {
            warn!(?resp, "cancel returned non-success status");
        }
        Ok(())
    }

    async fn send(&self, action: &ExchangeAction) -> Result<serde_json::Value> {
        let action_json = serde_json::to_value(action)?;
        let nonce = chrono::Utc::now().timestamp_millis() as u64;
        let signature: SignatureRsV =
            sign_action(&self.creds, &action_json, nonce, self.is_mainnet).await?;
        let body = SignedExchangeRequest {
            action: &action_json,
            nonce,
            signature,
            vault_address: self.creds.vault.clone(),
        };
        let resp = self
            .http
            .post(format!("{}/exchange", self.rest_url))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;
        Ok(resp)
    }

    pub fn address(&self) -> String {
        self.creds.address()
    }
}
