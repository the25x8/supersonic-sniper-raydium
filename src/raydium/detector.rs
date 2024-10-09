use std::str::FromStr;
use std::sync::{Arc};
use borsh::BorshDeserialize;
use base64::{Engine};
use chrono::{TimeZone, Utc};
use futures::{StreamExt, SinkExt};
use serde_json::json;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use solana_account_decoder::{UiAccountData, UiAccountEncoding};
use solana_client::nonblocking::pubsub_client::{PubsubClient};
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_client::rpc_filter::RpcFilterType::DataSize;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tokio::sync::mpsc::Sender;

use crate::raydium::{LiquidityStateV4, MainnetProgramId};
use crate::detector::{DetectedPool, Pool, PoolType, SourceType};
use crate::error::handle_attempt;

const MAX_SUBSCRIPTION_RETRIES: u32 = 5; // Maximum number of subscription retries
const RECONNECT_BACKOFF: u64 = 1000; // Reconnection backoff in milliseconds

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum BXPoolEvent {
    NewPool(BXNewPoolEvent),
    WsConnected(BXConnectedEvent),
    WsError(BXErrorEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BXConnectedEvent {
    pub id: i64,
    pub jsonrpc: String,
    pub result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BXErrorEvent {
    pub id: i64,
    pub jsonrpc: String,
    pub error: BXError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BXError {
    pub code: i64,
    pub message: String,
    pub data: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BXNewPoolEvent {
    pub jsonrpc: String,
    pub method: String,
    pub params: BXPoolParams,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BXPoolParams {
    pub subscription: String,
    pub result: BXPoolResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BXPoolResult {
    pub slot: String,
    pub pool: BXPoolInfo,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BXLiquidityPoolKeys {
    id: String,
    base_mint: String,
    quote_mint: String,
    lp_mint: String,
    version: u8,
    #[serde(rename = "programID")]
    program_id: String,
    authority: String,
    base_vault: String,
    quote_vault: String,
    lp_vault: String,
    open_orders: String,
    target_orders: String,
    withdraw_queue: String,
    market_version: u8,
    #[serde(rename = "marketProgramID")]
    market_program_id: String,
    #[serde(rename = "marketID")]
    market_id: String,
    market_authority: String,
    market_base_vault: String,
    market_quote_vault: String,
    market_bids: String,
    market_asks: String,
    market_event_queue: String,
    trade_fee_rate: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BXPoolInfo {
    pub pool: String,
    pub pool_address: String,
    pub token1_mint_address: String,
    pub token1_mint_symbol: String,
    pub token1_reserves: String,
    pub token2_mint_address: String,
    pub token2_mint_symbol: String,
    pub token2_reserves: String,
    pub open_time: String,
    pub pool_type: String,
    pub liquidity_pool_keys: BXLiquidityPoolKeys,
}

pub struct RaydiumDetector {
    pubsub_client: Arc<PubsubClient>,
    tx: Sender<DetectedPool>, // Detected pool channel
    bx_ws_url: Option<String>,
    bx_auth_token: Option<String>,
}

impl RaydiumDetector {
    pub async fn new(
        tx: Sender<DetectedPool>,
        rpc_ws_url: &str,
        bloxroute_ws_url: Option<&str>,
        bloxroute_auth_token: Option<&str>,
    ) -> Self {
        // Initialize the Pubsub client instance
        let pubsub_client = match PubsubClient::new(rpc_ws_url).await {
            Ok(client) => client,
            Err(e) => {
                panic!("Failed to create WebSocket client: {}", e);
            }
        };
        let pubsub_client_arc = Arc::new(pubsub_client);

        Self {
            tx,
            pubsub_client: pubsub_client_arc,
            bx_ws_url: bloxroute_ws_url.map(|s| s.to_string()),
            bx_auth_token: bloxroute_auth_token.map(|s| s.to_string()),
        }
    }

    pub async fn start(&self) {
        tokio::join!(
            self.subscribe_to_amm_pools_bloxroute(),
            self.subscribe_to_amm_pools_pubsub(),
        );

        // If all subscriptions fail, abort the program
        panic!("All Raydium pool subscriptions failed");
    }

    async fn subscribe_to_amm_pools_pubsub(&self) {
        // Clone the Pubsub client instance
        let pubsub = self.pubsub_client.clone();
        let mut attempts = 0;

        loop {
            // Combine the custom filters with common filters
            let filters = vec![
                // Filter by data size (752 bytes for Raydium LiquidityStateV4)
                DataSize(752),

                // Filter by status field with SwapOnly status
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    0, // Offset of status field
                    &[6, 0, 0, 0, 0, 0, 0, 0],
                )),

                // Filter by quote mint. Disable because token1/token2 mint can be swapped,
                // determine the base token after parsing the data.
                // RpcFilterType::Memcmp(Memcmp::new(
                //     432,
                //     MemcmpEncodedBytes::Base58(detector::WSOL_MINT.to_string()),
                // )),

                // Filter by market program ID
                // RpcFilterType::Memcmp(Memcmp::new(
                //     560,
                //     MemcmpEncodedBytes::Base58(MainnetProgramId::OpenbookMarket.get_pubkey().to_string()),
                // )),
            ];
            let config = RpcProgramAccountsConfig {
                filters: Some(filters),
                account_config: RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64),
                    commitment: Some(CommitmentConfig::processed()),
                    ..RpcAccountInfoConfig::default()
                },
                with_context: Some(false),
                sort_results: None,
            };

            // Subscribe to the amm program account stream
            let (mut accounts, _) =
                match pubsub.program_subscribe(&MainnetProgramId::AmmV4.get_pubkey(), Some(config)).await {
                    Ok((accounts, root_unsubscribe)) => (accounts, root_unsubscribe),
                    Err(e) => {
                        error!("Failed to subscribe to Raydium pool stream: {}", e.to_string());

                        // Try to reconnect, and return if max retries exceeded
                        if let Err(_) = handle_attempt(
                            &mut attempts,
                            MAX_SUBSCRIPTION_RETRIES,
                            RECONNECT_BACKOFF,
                        ).await {
                            error!("Max subscription retries for Raydium pool stream exceeded");
                            break;
                        }

                        // Retry the subscription
                        continue;
                    }
                };

            // Reset the connection attempts counter
            attempts = 0;

            // Set the start timestamp when the subscription is successful
            let start_timestamp = Utc::now();

            info!(
                "\n\
                🌊 Raydium Pool Stream\n\
                ▶️ Subscribed to Raydium Pool Stream via PubSub\n\
                ▶️ Waiting for new pools...\n\
                ───────────────────────────────────────"
            );

            // Continue processing the subscription
            while let Some(response) = accounts.next().await {
                // Read public key of amm pool
                let pubkey = match Pubkey::from_str(&response.value.pubkey) {
                    Ok(pk) => pk,
                    Err(e) => {
                        error!("Failed to parse pool pubkey: {}", e);
                        continue;
                    }
                };

                // Deserialize the liquidity state data from the account into a DetectedPool instance
                if let Err(e) = self.handle_account_update(pubkey, &response.value.account.data).await {
                    error!("Failed to handle account update: {}", e);
                }
            }

            // Wait before retrying to avoid busy looping
            info!(
                "\n🔄 [RECONNECT]\n\
                Resubscribing to Raydium pool stream via PubSub...\n\
                ───────────────────────────",
            );

            // Try to reconnect after a delay
            if let Err(_) = handle_attempt(
                &mut attempts,
                MAX_SUBSCRIPTION_RETRIES,
                RECONNECT_BACKOFF,
            ).await {
                error!("Max reconnection retries for Raydium pool stream exceeded");
                break;
            }
        }
    }

    async fn handle_account_update(&self, pubkey: Pubkey, data: &UiAccountData) -> Result<(), Box<dyn std::error::Error>> {
        // Deserialize the liquidity state data from the account into a DetectedPool instance
        match data {
            UiAccountData::Binary(data_base64, _encoding) => {
                if let Ok(data) = base64::prelude::BASE64_STANDARD.decode(data_base64) {
                    // Parse the liquidity state data
                    let state = match LiquidityStateV4::try_from_slice(&data) {
                        Ok(state) => state,
                        Err(e) => {
                            error!("Failed to parse liquidity state v4 data: {}", e);
                            return Err(e.into());
                        }
                    };

                    // Build the DetectedPool instance
                    let pool_open_time = Utc.timestamp_opt(state.state_data.pool_open_time as i64, 0).unwrap();
                    let pool_info = Pool {
                        pubkey,
                        in_whitelist: false,
                        slot: 0,
                        pool_address: pubkey.to_string(),
                        pool_type: PoolType::RaydiumAmm,
                        base_decimals: Some(state.coin_decimals as u8),
                        base_mint: state.coin_vault_mint,
                        base_vault: state.coin_vault,
                        base_reserves: None, // Not available for PubSub
                        quote_decimals: Some(state.pc_decimals as u8),
                        quote_mint: state.pc_vault_mint,
                        quote_vault: state.pc_vault,
                        quote_reserves: None, // Not available for PubSub
                        status: state.status,
                        open_time: pool_open_time,
                        timestamp: Utc::now(),
                    };

                    // Send the detected pool to the channel
                    if let Err(e) = self.tx.send(DetectedPool {
                        start_timestamp: Utc::now(),
                        data: pool_info,
                        source: SourceType::PubSub,
                    }).await {
                        error!("Failed to send new pool to the detector channel: {:?}", e);
                    }
                } else {
                    error!("Failed to decode base64 data for account {}", pubkey);
                }
            }
            _ => error!("Unexpected data format for account {}", pubkey),
        }

        Ok(())
    }

    async fn subscribe_to_amm_pools_bloxroute(&self) {
        let mut attempts = 0;

        loop {
            // If ws url is set, auth token is required
            let ws_url = self.bx_ws_url.clone();
            let auth_token = self.bx_auth_token.clone();

            if ws_url.is_none() {
                warn!("Bloxroute WebSocket URL not set, skipping Bloxroute subscription...");
                break;
            }
            if auth_token.is_none() {
                error!("Bloxroute WebSocket URL is set, but no auth token provided");
                break;
            }

            // Prepare the subscription request
            let subscription_request = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "subscribe",
                "params": ["GetNewRaydiumPoolsStream", { "includeCPMM": false }]
            });

            // Convert the URL into a WebSocket Request
            let mut request = match ws_url.unwrap().to_string().into_client_request() {
                Ok(req) => req,
                Err(e) => {
                    error!("Invalid Bloxroute WebSocket URL: {}", e);
                    break;
                }
            };

            // Add the Authorization header
            request.headers_mut().append("Authorization", http::HeaderValue::from_str(auth_token.unwrap().as_str()).unwrap());

            // Connect to WebSocket
            let ws_stream = match connect_async(request).await {
                Ok((ws_stream, _)) => ws_stream,
                Err(e) => {
                    error!("Failed to connect to Bloxroute WebSocket: {}", e);

                    // Try to reconnect after a delay
                    if let Err(_) = handle_attempt(
                        &mut attempts,
                        MAX_SUBSCRIPTION_RETRIES,
                        RECONNECT_BACKOFF,
                    ).await {
                        error!("Max connection retries for Bloxroute WebSocket exceeded");
                        break;
                    }

                    // Retry the connection
                    continue;
                }
            };

            // Set the start timestamp when the subscription starts
            let start_timestamp = Utc::now();

            // Reset the connection attempts counter
            attempts = 0;

            // Split the WebSocket stream into sender and receiver
            let (mut write, mut read) = ws_stream.split();

            // Send the subscription request
            if let Err(e) = write.send(Message::Text(subscription_request.to_string())).await {
                error!("Failed to subscribe to Raydium pool stream via Bloxroute: {}", e);
                break;
            }

            // Loop to receive new pool events
            while let Some(message_result) = read.next().await {
                let message = match message_result {
                    Ok(msg) => msg,
                    Err(e) => {
                        error!("Failed to receive message from Bloxroute WebSocket: {}", e);
                        break;
                    }
                };


                // Process the message
                match message {
                    Message::Text(text) => {
                        // Attempt to deserialize into BXPoolEvent
                        let event = match serde_json::from_str::<BXPoolEvent>(&text) {
                            Ok(event) => event,
                            Err(e) => {
                                error!("Failed to parse Raydium pool event: {}", e);
                                continue;
                            }
                        };

                        // Process the event
                        match event {
                            BXPoolEvent::NewPool(event) => {
                                // Extract the pool data
                                let pool = event.params.result.pool;
                                let pubkey = match Pubkey::from_str(pool.pool_address.as_str()) {
                                    Ok(pk) => pk,
                                    Err(e) => {
                                        error!("Failed to parse pool pubkey: {}", e);
                                        continue;
                                    }
                                };

                                // Build the DetectedPool instance
                                let pool_info = Pool {
                                    pubkey,
                                    in_whitelist: false,
                                    slot: event.params.result.slot.parse().unwrap_or(0),
                                    pool_address: pool.pool_address,
                                    pool_type: PoolType::RaydiumAmm,
                                    status: 6, // Assume SwapOnly status
                                    base_decimals: None, // Not available for PubSub
                                    base_mint: Pubkey::from_str(pool.token1_mint_address.as_str()).unwrap(),
                                    base_vault: Pubkey::from_str(pool.liquidity_pool_keys.base_vault.as_str()).unwrap(),
                                    base_reserves: Some(pool.token1_reserves.parse().unwrap_or(0)),
                                    quote_decimals: None, // Not available for PubSub
                                    quote_mint: Pubkey::from_str(pool.token2_mint_address.as_str()).unwrap(),
                                    quote_vault: Pubkey::from_str(pool.liquidity_pool_keys.quote_vault.as_str()).unwrap(),
                                    quote_reserves: Some(pool.token2_reserves.parse().unwrap_or(0)),
                                    open_time: Utc.timestamp_opt(pool.open_time.parse().unwrap_or(0), 0).unwrap(),
                                    timestamp: Utc::now(),
                                };

                                // Send the detected pool to the channel
                                if let Err(e) = self.tx.send(DetectedPool {
                                    start_timestamp,
                                    data: pool_info,
                                    source: SourceType::BloxRoute,
                                }).await {
                                    error!("Failed to send new pool to the detector channel: {:?}", e);
                                }
                            }
                            BXPoolEvent::WsConnected(event) => {
                                info!(
                                    "\n\
                                    🌊 Raydium Pool Stream\n\
                                    ▶️ Subscribed to Raydium Pool Stream via BloxRoute\n\
                                    ▶️ Waiting for new pools...\n\
                                    ───────────────────────────────────────"
                                );
                            }
                            BXPoolEvent::WsError(event) => {
                                error!("Bloxroute WebSocket error: {}", event.error.message);
                                break;
                            }
                        }
                    }
                    Message::Ping(ping) => {
                        // Respond to pings to keep the connection alive
                        if let Err(e) = write.send(Message::Pong(ping)).await {
                            error!("Failed to send Pong message: {}", e);
                            break;
                        }
                    }
                    Message::Close(frame) => {
                        break;
                    }
                    _ => {
                        continue;
                    }
                }
            }

            info!(
                "\n🔄 [RECONNECT]\n\
                Resubscribing to Raydium pool stream via Bloxroute...\n\
                ───────────────────────────",
            );

            // Try to reconnect after a delay
            if let Err(_) = handle_attempt(
                &mut attempts,
                MAX_SUBSCRIPTION_RETRIES,
                RECONNECT_BACKOFF,
            ).await {
                error!("Max reconnection retries for Bloxroute WebSocket exceeded");
                return;
            }
        }
    }
}

