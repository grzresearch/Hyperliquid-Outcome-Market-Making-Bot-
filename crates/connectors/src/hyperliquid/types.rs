use serde::{Deserialize, Serialize};

/// Wire-format subscription for the Hyperliquid websocket.
///
/// Examples used by this bot:
/// ```json
/// {"method":"subscribe","subscription":{"type":"l2Book","coin":"BTC","nSigFigs":5}}
/// {"method":"subscribe","subscription":{"type":"l2Book","coin":"OUT:BTC-78213-2026-05-03-YES"}}
/// {"method":"subscribe","subscription":{"type":"trades","coin":"BTC"}}
/// {"method":"subscribe","subscription":{"type":"userEvents","user":"0xabc..."}}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsRequest {
    pub method: String,
    pub subscription: WsSubscription,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WsSubscription {
    L2Book {
        coin: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        n_sig_figs: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        n_levels: Option<u32>,
    },
    Trades {
        coin: String,
    },
    /// Bbo only — cheapest possible top-of-book stream.
    Bbo {
        coin: String,
    },
    UserEvents {
        user: String,
    },
    OrderUpdates {
        user: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "channel", rename_all = "camelCase")]
pub enum WsResponse {
    SubscriptionResponse(serde_json::Value),
    Pong,
    L2Book(L2BookPayload),
    Trades(Vec<TradePayload>),
    Bbo(BboPayload),
    UserEvents(UserEventsPayload),
    OrderUpdates(Vec<OrderUpdatePayload>),
    Error(serde_json::Value),
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct L2BookPayload {
    pub coin: String,
    pub time: i64,
    /// `levels[0]` = bids, `levels[1]` = asks. Each level: { px, sz, n }.
    pub levels: [Vec<RawLevel>; 2],
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawLevel {
    pub px: String,
    pub sz: String,
    pub n: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BboPayload {
    pub coin: String,
    pub time: i64,
    pub bbo: [Option<RawLevel>; 2],
}

#[derive(Debug, Clone, Deserialize)]
pub struct TradePayload {
    pub coin: String,
    pub side: String,
    pub px: String,
    pub sz: String,
    pub time: i64,
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserEventsPayload {
    #[serde(default)]
    pub fills: Vec<UserFill>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserFill {
    pub coin: String,
    pub px: String,
    pub sz: String,
    pub side: String,
    pub time: i64,
    pub fee: String,
    pub liquidation: Option<bool>,
    pub liquidity: Option<String>,
    pub oid: u64,
    pub cloid: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderUpdatePayload {
    pub order: OrderUpdateOrder,
    pub status: String,
    pub status_timestamp: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderUpdateOrder {
    pub coin: String,
    pub side: String,
    pub limit_px: String,
    pub sz: String,
    pub oid: u64,
    pub timestamp: i64,
    pub orig_sz: String,
    pub cloid: Option<String>,
}

/// Exchange action — placed orders, cancels, etc. The wire format uses
/// `msgpack` + `keccak256` for the action hash; see `signing.rs` for details.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ExchangeAction {
    Order(OrderAction),
    Cancel(CancelAction),
    CancelByCloid(CancelByCloidAction),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderAction {
    pub orders: Vec<OrderRequest>,
    pub grouping: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderRequest {
    pub a: u32,            // asset index
    pub b: bool,           // is_buy
    pub p: String,         // price
    pub s: String,         // size
    pub r: bool,           // reduce_only
    pub t: OrderTrigger,   // tif / trigger
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c: Option<String>, // cloid (hex 0x...)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderTrigger {
    pub limit: LimitTif,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitTif {
    pub tif: String, // "Gtc" | "Ioc" | "Alo"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelAction {
    pub cancels: Vec<CancelRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelRequest {
    pub a: u32,
    pub o: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelByCloidAction {
    pub cancels: Vec<CancelByCloidRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelByCloidRequest {
    pub asset: u32,
    pub cloid: String,
}

/// Top-level signed exchange envelope.
#[derive(Debug, Clone, Serialize)]
pub struct SignedExchangeRequest<'a> {
    pub action: &'a serde_json::Value,
    pub nonce: u64,
    pub signature: SignatureRsV,
    #[serde(rename = "vaultAddress", skip_serializing_if = "Option::is_none")]
    pub vault_address: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureRsV {
    pub r: String,
    pub s: String,
    pub v: u8,
}

/// Hyperliquid `meta` response (perp universe).
#[derive(Debug, Clone, Deserialize)]
pub struct PerpMeta {
    pub universe: Vec<PerpAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PerpAsset {
    pub name: String,
    pub sz_decimals: u32,
    pub max_leverage: u32,
}

/// `outcomeMeta` response (HIP-4 universe). The exact field names mirror the
/// Hyperliquid GitBook docs as of the HIP-4 mainnet launch on May 2, 2026.
#[derive(Debug, Clone, Deserialize)]
pub struct OutcomeMeta {
    pub markets: Vec<OutcomeMetaMarket>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeMetaMarket {
    pub name: String,
    pub yes_coin: String,
    pub no_coin: String,
    pub underlying: String,
    pub strike: String,
    pub direction: String, // "above" or "below"
    pub expiry: i64,       // ns since epoch
    pub settlement_asset: String, // "USDH"
    pub tick_size: String,
    pub min_size: String,
}
