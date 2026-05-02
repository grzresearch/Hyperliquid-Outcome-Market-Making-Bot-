use anyhow::{Context, Result};
use base64::Engine;
use chrono::Utc;
use hl_omm_core::{ClientOrderId, MarketKey, Order, Side};
use parking_lot::RwLock;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Client;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::pss::SigningKey;
use rsa::sha2::Sha256;
use rsa::signature::{RandomizedSigner, SignatureEncoding};
use rsa::RsaPrivateKey;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

#[derive(Clone, Debug)]
pub struct KalshiCreds {
    pub key_id: String,
    pub private_key: Arc<RsaPrivateKey>,
}

impl KalshiCreds {
    pub fn from_pem(key_id: String, pem: &str) -> Result<Self> {
        let pk = RsaPrivateKey::from_pkcs8_pem(pem)
            .or_else(|_| RsaPrivateKey::from_pkcs1_pem(pem))
            .context("decode kalshi RSA key")?;
        Ok(Self {
            key_id,
            private_key: Arc::new(pk),
        })
    }
}

#[derive(Clone)]
pub struct KalshiClient {
    rest_url: String,
    creds: Arc<RwLock<KalshiCreds>>,
    http: Client,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KalshiMarket {
    pub ticker: String,
    pub event_ticker: String,
    pub title: String,
    pub yes_bid: Option<i64>,
    pub yes_ask: Option<i64>,
    pub no_bid: Option<i64>,
    pub no_ask: Option<i64>,
    pub close_time: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
struct PlaceOrderBody<'a> {
    ticker: &'a str,
    client_order_id: String,
    side: &'a str, // "yes" | "no"
    action: &'a str, // "buy" | "sell"
    count: i64,
    yes_price: Option<i64>,
    no_price: Option<i64>,
    r#type: &'a str, // "limit" | "market"
    expiration_ts: Option<i64>,
}

impl KalshiClient {
    pub fn new(creds: KalshiCreds) -> Self {
        let http = Client::builder()
            .pool_idle_timeout(Some(Duration::from_secs(30)))
            .timeout(Duration::from_secs(5))
            .tcp_nodelay(true)
            .build()
            .expect("reqwest builder");
        Self {
            rest_url: super::REST_URL.into(),
            creds: Arc::new(RwLock::new(creds)),
            http,
        }
    }

    pub async fn search_markets(&self, query: &str) -> Result<Vec<KalshiMarket>> {
        let url = format!("{}/markets?status=open&limit=200", self.rest_url);
        let resp: serde_json::Value = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let markets: Vec<KalshiMarket> = serde_json::from_value(
            resp.get("markets").cloned().unwrap_or(serde_json::Value::Array(vec![])),
        )?;
        Ok(markets
            .into_iter()
            .filter(|m| m.title.to_lowercase().contains(&query.to_lowercase()))
            .collect())
    }

    pub async fn get_orderbook(&self, ticker: &str) -> Result<serde_json::Value> {
        let path = format!("/markets/{}/orderbook", ticker);
        let v = self
            .http
            .get(format!("{}{}", self.rest_url, path))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(v)
    }

    pub async fn place_order(&self, market: &MarketKey, order: &Order) -> Result<String> {
        // Kalshi quotes prices as integer cents in [1, 99].
        let price_cents = (order.price.0 * rust_decimal::Decimal::from(100))
            .round()
            .to_string()
            .parse::<i64>()
            .unwrap_or(0);
        let count = order
            .qty
            .0
            .round()
            .to_string()
            .parse::<i64>()
            .unwrap_or(0);
        let action = match order.side {
            Side::Buy => "buy",
            Side::Sell => "sell",
        };
        // Outcome side ("yes" / "no") is encoded in the Kalshi-side ticker.
        let yes_side = market.instrument.0.to_uppercase().contains("-YES");
        let body = PlaceOrderBody {
            ticker: &market.instrument.0,
            client_order_id: order.cloid.to_hex(),
            side: if yes_side { "yes" } else { "no" },
            action,
            count,
            yes_price: if yes_side { Some(price_cents) } else { None },
            no_price: if yes_side { None } else { Some(price_cents) },
            r#type: "limit",
            expiration_ts: None,
        };
        let body_json = serde_json::to_string(&body)?;
        let path = "/portfolio/orders";
        let headers = self.signed_headers("POST", path, &body_json)?;
        let resp = self
            .http
            .post(format!("{}{}", self.rest_url, path))
            .headers(headers)
            .body(body_json)
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;
        debug!(?resp, "kalshi place_order response");
        Ok(resp
            .pointer("/order/order_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default())
    }

    pub async fn cancel(&self, order_id: &str) -> Result<()> {
        let path = format!("/portfolio/orders/{}", order_id);
        let headers = self.signed_headers("DELETE", &path, "")?;
        self.http
            .delete(format!("{}{}", self.rest_url, path))
            .headers(headers)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn cancel_by_cloid(&self, _market: &MarketKey, _cloid: ClientOrderId) -> Result<()> {
        // Kalshi only cancels by their own order_id; the bot maintains a
        // cloid -> order_id map in the strategy layer.
        Ok(())
    }

    fn signed_headers(&self, method: &str, path: &str, _body: &str) -> Result<HeaderMap> {
        let creds = self.creds.read();
        let timestamp = Utc::now().timestamp_millis().to_string();
        let to_sign = format!("{}{}{}", timestamp, method, path);
        let signing_key = SigningKey::<Sha256>::new(creds.private_key.as_ref().clone());
        let mut rng = rand::thread_rng();
        let sig = signing_key.sign_with_rng(&mut rng, to_sign.as_bytes());
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());

        let mut h = HeaderMap::new();
        h.insert("KALSHI-ACCESS-KEY", HeaderValue::from_str(&creds.key_id)?);
        h.insert("KALSHI-ACCESS-TIMESTAMP", HeaderValue::from_str(&timestamp)?);
        h.insert("KALSHI-ACCESS-SIGNATURE", HeaderValue::from_str(&sig_b64)?);
        h.insert("Content-Type", HeaderValue::from_static("application/json"));
        Ok(h)
    }
}
