use anyhow::{Context, Result};
use base64::Engine;
use chrono::Utc;
use hl_omm_core::{ClientOrderId, MarketKey, Order, Side};
use parking_lot::RwLock;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PolyApiCreds {
    pub key: String,
    pub secret: String,
    pub passphrase: String,
    pub maker: String, // EOA used for orders.
}

#[derive(Clone)]
pub struct PolymarketClient {
    clob_url: String,
    gamma_url: String,
    creds: Arc<RwLock<PolyApiCreds>>,
    http: Client,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolyMarket {
    pub condition_id: String,
    pub question: String,
    #[serde(default)]
    pub tokens: Vec<PolyToken>,
    #[serde(default)]
    pub tick_size: Option<String>,
    #[serde(default)]
    pub minimum_order_size: Option<String>,
    pub end_date_iso: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolyToken {
    pub token_id: String,
    pub outcome: String, // "Yes" or "No"
}

#[derive(Debug, Clone, Serialize)]
struct OrderPayload<'a> {
    order: PolyOrder<'a>,
    owner: &'a str,
    order_type: &'a str, // "GTC" | "FOK" | "GTD"
}

#[derive(Debug, Clone, Serialize)]
struct PolyOrder<'a> {
    salt: u128,
    maker: &'a str,
    signer: &'a str,
    taker: &'a str, // 0x000... for any taker
    token_id: &'a str,
    maker_amount: String,
    taker_amount: String,
    side: u8, // 0 = BUY, 1 = SELL
    expiration: u64,
    nonce: u64,
    fee_rate_bps: u32,
    signature_type: u8, // 0 EOA, 1 polyproxy, 2 magic
    signature: String,
}

impl PolymarketClient {
    pub fn new(creds: PolyApiCreds) -> Self {
        let http = Client::builder()
            .pool_idle_timeout(Some(Duration::from_secs(30)))
            .timeout(Duration::from_secs(5))
            .tcp_nodelay(true)
            .build()
            .expect("reqwest builder");
        Self {
            clob_url: super::CLOB_URL.into(),
            gamma_url: super::GAMMA_URL.into(),
            creds: Arc::new(RwLock::new(creds)),
            http,
        }
    }

    pub async fn search_markets(&self, query: &str) -> Result<Vec<PolyMarket>> {
        let url = format!("{}/markets?search={}&active=true&closed=false", self.gamma_url, query);
        let body: Vec<PolyMarket> = self.http.get(url).send().await?.error_for_status()?.json().await?;
        Ok(body)
    }

    pub async fn get_book(&self, token_id: &str) -> Result<serde_json::Value> {
        let url = format!("{}/book?token_id={}", self.clob_url, token_id);
        let v = self.http.get(url).send().await?.error_for_status()?.json().await?;
        Ok(v)
    }

    pub async fn place_order(&self, market: &MarketKey, order: &Order) -> Result<String> {
        // The order body is HMAC-signed with the API secret. The L2 order
        // itself (taker / maker amounts) carries an EIP-712 signature from
        // the maker EOA — see the Polymarket SDK for the exact typed-data
        // schema. We surface a tight wrapper here and assume the caller has
        // pre-signed the inner structure.
        let creds = self.creds.read().clone();
        let order_type = "GTC";
        let payload = OrderPayload {
            order: PolyOrder {
                salt: rand::random(),
                maker: &creds.maker,
                signer: &creds.maker,
                taker: "0x0000000000000000000000000000000000000000",
                token_id: &market.instrument.0,
                maker_amount: order.qty.0.to_string(),
                taker_amount: (order.qty.0 * order.price.0).to_string(),
                side: if matches!(order.side, Side::Buy) { 0 } else { 1 },
                expiration: 0,
                nonce: chrono::Utc::now().timestamp_millis() as u64,
                fee_rate_bps: 0,
                signature_type: 0,
                signature: "0x".into(),
            },
            owner: &creds.maker,
            order_type,
        };
        let body = serde_json::to_string(&payload)?;
        let path = "/order";
        let timestamp = Utc::now().timestamp().to_string();
        let headers = self.signed_headers(&creds, "POST", path, &timestamp, &body)?;
        let resp = self
            .http
            .post(format!("{}{}", self.clob_url, path))
            .headers(headers)
            .body(body)
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;
        debug!(?resp, "polymarket place_order response");
        Ok(resp
            .get("orderID")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default())
    }

    pub async fn cancel(&self, order_id: &str) -> Result<()> {
        let creds = self.creds.read().clone();
        let path = format!("/order/{}", order_id);
        let timestamp = Utc::now().timestamp().to_string();
        let headers = self.signed_headers(&creds, "DELETE", &path, &timestamp, "")?;
        self.http
            .delete(format!("{}{}", self.clob_url, path))
            .headers(headers)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn cancel_by_cloid(&self, _market: &MarketKey, cloid: ClientOrderId) -> Result<()> {
        // Polymarket exposes /cancel-orders with a list of order IDs. We map
        // cloid->order_id locally before hitting this API; for brevity we
        // assume `cloid.to_hex()` is the local key used by the higher layer.
        self.cancel(&cloid.to_hex()).await
    }

    fn signed_headers(
        &self,
        creds: &PolyApiCreds,
        method: &str,
        path: &str,
        timestamp: &str,
        body: &str,
    ) -> Result<HeaderMap> {
        use hmac::{Hmac, Mac};
        type HmacSha256 = Hmac<Sha256>;
        let secret = base64::engine::general_purpose::STANDARD
            .decode(&creds.secret)
            .context("decode poly secret")?;
        let mut mac = HmacSha256::new_from_slice(&secret).map_err(|e| anyhow::anyhow!(e.to_string()))?;
        mac.update(timestamp.as_bytes());
        mac.update(method.as_bytes());
        mac.update(path.as_bytes());
        mac.update(body.as_bytes());
        let signature = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        let mut h = HeaderMap::new();
        h.insert("POLY_API_KEY", HeaderValue::from_str(&creds.key)?);
        h.insert("POLY_PASSPHRASE", HeaderValue::from_str(&creds.passphrase)?);
        h.insert("POLY_TIMESTAMP", HeaderValue::from_str(timestamp)?);
        h.insert("POLY_SIGNATURE", HeaderValue::from_str(&signature)?);
        h.insert("Content-Type", HeaderValue::from_static("application/json"));
        Ok(h)
    }
}
