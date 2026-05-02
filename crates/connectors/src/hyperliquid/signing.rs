//! EIP-712 signing for Hyperliquid exchange actions.
//!
//! The Hyperliquid contract serialises actions with `msgpack`, hashes the
//! result with `keccak256`, prepends the connection id (mainnet = `"a"`,
//! testnet = `"b"`) and signs the resulting hash with the user's wallet via
//! EIP-712. See `hyperliquid-python-sdk/hyperliquid/utils/signing.py` for the
//! reference implementation.

use anyhow::{Context, Result};
use ethers::core::types::transaction::eip712::EIP712Domain;
use ethers::signers::{LocalWallet, Signer};
use ethers::types::H256;
use serde::Serialize;
use sha3::{Digest, Keccak256};

use super::types::SignatureRsV;

#[derive(Clone, Debug)]
pub struct ApiCreds {
    pub wallet: LocalWallet,
    /// Optional vault / sub-account; if set, all orders route through it.
    pub vault: Option<String>,
}

impl ApiCreds {
    pub fn from_private_key(pk_hex: &str, vault: Option<String>) -> Result<Self> {
        let wallet: LocalWallet = pk_hex
            .trim_start_matches("0x")
            .parse()
            .context("invalid private key")?;
        Ok(Self { wallet, vault })
    }

    pub fn address(&self) -> String {
        format!("{:?}", self.wallet.address())
    }
}

/// Compute the action hash exactly the way the Hyperliquid contract does.
pub fn action_hash(action_msgpack: &[u8], nonce: u64, vault: Option<&str>) -> H256 {
    let mut hasher = Keccak256::new();
    hasher.update(action_msgpack);
    hasher.update(nonce.to_be_bytes());
    if let Some(v) = vault {
        hasher.update([1u8]);
        hasher.update(hex::decode(v.trim_start_matches("0x")).unwrap_or_default());
    } else {
        hasher.update([0u8]);
    }
    H256::from_slice(&hasher.finalize())
}

pub fn encode_action(action: &serde_json::Value) -> Result<Vec<u8>> {
    let canonical = canonicalise(action);
    rmp_serde_encode(&canonical)
}

fn canonicalise(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), canonicalise(&map[&k]));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(canonicalise).collect())
        }
        _ => v.clone(),
    }
}

/// Build the EIP-712 typed-data digest the Hyperliquid contract expects:
///   domain  = { name: "Exchange", version: "1", chainId: 1337, verifyingContract: 0x000...0 }
///   types   = Agent(string source, bytes32 connectionId)
fn typed_digest(source: &str, connection_id: H256) -> [u8; 32] {
    let domain = EIP712Domain {
        name: Some("Exchange".into()),
        version: Some("1".into()),
        chain_id: Some(1337u64.into()),
        verifying_contract: Some(ethers::types::Address::zero()),
        salt: None,
    };
    let domain_separator: [u8; 32] = domain.separator();

    let type_hash: [u8; 32] = Keccak256::digest(b"Agent(string source,bytes32 connectionId)").into();

    let mut struct_h = Keccak256::new();
    struct_h.update(type_hash);
    struct_h.update(Keccak256::digest(source.as_bytes()));
    struct_h.update(connection_id.as_bytes());
    let struct_hash: [u8; 32] = struct_h.finalize().into();

    let mut h = Keccak256::new();
    h.update([0x19u8, 0x01u8]);
    h.update(domain_separator);
    h.update(struct_hash);
    h.finalize().into()
}

pub async fn sign_action(
    creds: &ApiCreds,
    action: &serde_json::Value,
    nonce: u64,
    is_mainnet: bool,
) -> Result<SignatureRsV> {
    let msgpack = encode_action(action)?;
    let conn_id = action_hash(&msgpack, nonce, creds.vault.as_deref());
    let source = if is_mainnet { "a" } else { "b" };
    let digest = typed_digest(source, conn_id);
    let sig = creds.wallet.sign_hash(H256::from(digest))?;
    Ok(SignatureRsV {
        r: format!("0x{:x}", sig.r),
        s: format!("0x{:x}", sig.s),
        v: sig.v as u8,
    })
}

fn rmp_serde_encode(v: &serde_json::Value) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(256);
    v.serialize(&mut rmp_serde::Serializer::new(&mut buf).with_struct_map())
        .context("msgpack encode")?;
    Ok(buf)
}
