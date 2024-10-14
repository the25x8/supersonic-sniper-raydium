use std::collections::HashSet;
use std::error::Error;
use std::str::FromStr;
use std::sync::Arc;
use borsh::BorshDeserialize;
use base64::Engine;
use chrono::{TimeZone, Utc};
use dashmap::DashSet;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use solana_account_decoder::{UiAccountData, UiAccountEncoding};
use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig, RpcTransactionConfig, RpcTransactionLogsConfig, RpcTransactionLogsFilter};
use solana_client::rpc_filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType};
use solana_client::rpc_filter::RpcFilterType::DataSize;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_sdk::transaction::VersionedTransaction;
use solana_transaction_status::{EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction, UiInstruction, UiMessage, UiTransaction, UiTransactionEncoding};
use tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tokio::sync::mpsc::Sender;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use crate::config::BloxrouteConfig;
use crate::raydium::MainnetProgramId;
use crate::detector::{DetectedPool, Pool, PoolKeys, PoolType, SourceType};
use crate::error::handle_attempt;
use crate::raydium::liquidity::LiquidityStateV4;

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
    rpc_client: Arc<RpcClient>,
    bloxroute_config: BloxrouteConfig,
    cancel_token: CancellationToken,
    initialized_pools: Arc<DashSet<Pubkey>>, // Pool initialized via initialize2
    tx: Sender<DetectedPool>, // Detected pool channel
}

impl RaydiumDetector {
    pub async fn new(
        rpc_client: Arc<RpcClient>,
        tx: Sender<DetectedPool>,
        initialized_cache: Arc<DashSet<Pubkey>>,
        rpc_ws_url: &str,
        bloxroute_config: &BloxrouteConfig,
        cancel_token: CancellationToken,
    ) -> Self {
        // Initialize the Pubsub client instance
        let pubsub_client = match PubsubClient::new(rpc_ws_url).await {
            Ok(client) => client,
            Err(e) => {
                cancel_token.cancel(); // Gracefully exit the bot
                panic!("Failed to create WebSocket client: {}", e);
            }
        };
        let pubsub_client_arc = Arc::new(pubsub_client);

        Self {
            tx,
            rpc_client,
            cancel_token,
            initialized_pools: initialized_cache,
            bloxroute_config: bloxroute_config.clone(),
            pubsub_client: pubsub_client_arc,
        }
    }

    pub async fn start(&self) {
        // If Bloxroute is enabled, subscribe to both PubSub and Bloxroute streams
        if self.bloxroute_config.enabled {
            tokio::join!(
                self.subscribe_to_amm_pool_initialize(),
                self.subscribe_to_amm_pools_pubsub(),
                self.subscribe_to_amm_pools_bloxroute(),
            );
            return;
        }

        // Otherwise, subscribe to the initialize2 logs and PubSub stream
        tokio::join!(
            self.subscribe_to_amm_pool_initialize(),
            self.subscribe_to_amm_pools_pubsub(),
        );
    }

    /// This method is used to subscribe to the initialize2 logs for Raydium AMM pools.
    /// It is used to detect new pools that are created on the Raydium AMM program
    /// regardless of the pool open time that can be not set or incorrect.
    /// It stores initialized pool key and in memory cache that can be used to
    /// detect new pools and their open time.
    async fn subscribe_to_amm_pool_initialize(&self) {
        // Clone the Pubsub client instance
        let pubsub = self.pubsub_client.clone();
        let rpc_client = self.rpc_client.clone();
        let cancel_token = self.cancel_token.clone();
        let mut attempts = 0;

        loop {
            let (mut logs, logs_unsubscribe) =
                match pubsub.logs_subscribe(
                    RpcTransactionLogsFilter::All,
                    RpcTransactionLogsConfig {
                        commitment: Some(CommitmentConfig::processed()),
                    },
                ).await {
                    Ok((accounts, logs_unsubscribe)) => (accounts, logs_unsubscribe),
                    Err(e) => {
                        error!("Failed to subscribe to Raydium Initialize2 logs: {}", e.to_string());

                        // Try to reconnect, and return if max retries exceeded
                        if let Err(_) = handle_attempt(
                            &mut attempts,
                            MAX_SUBSCRIPTION_RETRIES,
                            RECONNECT_BACKOFF,
                        ).await {
                            error!("Max subscription retries for Raydium Initialize2 logs exceeded");
                            cancel_token.cancel(); // Gracefully exit the bot
                            return;
                        }

                        // Retry the subscription
                        continue;
                    }
                };

            // Reset the connection attempts counter
            attempts = 0;

            tokio::select! {
                // Handle the logs subscription stream
                _ = async {
                    info!(
                        "\n\
                        ▶️ Subscribed to Raydium Initialize2 Logs via PubSub\n\
                        ▶️ Waiting for new initialized pools...\n\
                        ───────────────────────────────────────\n"
                    );

                    while let Some(response) = logs.next().await {
                        // You will need to adjust this according to how the pool address is stored in the logs
                        // Assuming the pool address is logged in a specific format, this is a placeholder
                        for log in &response.value.logs {
                            // If the log contains the initialize2 keyword try to get the pool pubkey
                            if log.contains("initialize2") {
                                let pool_pubkey = match try_to_load_initialized_pool(
                                    rpc_client.clone(),
                                    &response.value.signature,
                                ).await {
                                    Ok(pool_pubkey) => match pool_pubkey {
                                        Some(pk) => pk,
                                        None => continue, // Skip if no pool pubkey found
                                    },
                                    Err(e) => {
                                        error!("Failed to process initialize2 transaction: {}", e);
                                        continue;
                                    }
                                };

                                // Add the initialized pool to the cache
                                self.initialized_pools.insert(pool_pubkey);
                            }
                        }
                    }
                } => {}

                // Unsubscribe and exit the task if cancelled
                _ = cancel_token.cancelled() => {
                    info!("Unsubscribing from Raydium initialize2 logs via PubSub");
                    logs_unsubscribe().await; // Unsubscribe from the logs
                    break;
                }

                else => break,
            }
        }
    }

    /// Subscribe to the Raydium AMM pools via solana sdk PubSub.
    /// It implements reconnect logic with backoff and max retries.
    /// PubSub is the base subscription method for detector,
    /// if it fails the bot should be gracefully exited.
    async fn subscribe_to_amm_pools_pubsub(&self) {
        // Clone the Pubsub client instance
        let pubsub = self.pubsub_client.clone();
        let cancel_token = self.cancel_token.clone();

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
                //     MemcmpEncodedBytes::Base58(spl_token::native_mint::id().to_string()),
                // )),

                // Filter by market program ID
                RpcFilterType::Memcmp(Memcmp::new(
                    560,
                    MemcmpEncodedBytes::Base58(MainnetProgramId::OpenbookMarket.get_pubkey().to_string()),
                )),
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
            let (mut accounts, program_unsubscribe) =
                match pubsub.program_subscribe(&MainnetProgramId::AmmV4.get_pubkey(), Some(config)).await {
                    Ok((accounts, program_unsubscribe)) => (accounts, program_unsubscribe),
                    Err(e) => {
                        error!("Failed to subscribe to Raydium pool stream: {}", e.to_string());

                        // Try to reconnect, and return if max retries exceeded
                        if let Err(_) = handle_attempt(
                            &mut attempts,
                            MAX_SUBSCRIPTION_RETRIES,
                            RECONNECT_BACKOFF,
                        ).await {
                            error!("Max subscription retries for Raydium pool via PubSub stream exceeded");
                            cancel_token.cancel(); // Gracefully exit the bot
                            return;
                        }

                        // Retry the subscription
                        continue;
                    }
                };

            // Reset the connection attempts counter
            attempts = 0;

            tokio::select! {
                // Handle the program subscription stream
                _ = async {
                    info!(
                        "\n\
                        🌊 Raydium Pool Stream\n\
                        ▶️ Subscribed to Raydium Pool Stream via PubSub\n\
                        ▶️ Waiting for new pools...\n\
                        ───────────────────────────────────────\n"
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
                } => {}

                // Unsubscribe and exit the task if cancelled
                _ = cancel_token.cancelled() => {
                    info!("Unsubscribing from Raydium pool stream via PubSub");
                    program_unsubscribe().await; // Unsubscribe from the program
                    break;
                }

                else => break,
            }

            // Wait before retrying to avoid busy looping
            info!(
                "\n\
                🔄 [RECONNECT]\n\
                Resubscribing to Raydium pool stream via PubSub...\n\
                ───────────────────────────\n",
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

    async fn handle_account_update(&self, pubkey: Pubkey, data: &UiAccountData) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

                    // Build pool keys
                    let pool_keys = PoolKeys {
                        id: pubkey,
                        base_mint: state.coin_vault_mint,
                        quote_mint: state.pc_vault_mint,
                        lp_mint: state.lp_mint,
                        version: 4, // Raydium AMM version
                        program_id: MainnetProgramId::AmmV4.get_pubkey(),
                        authority: state.amm_owner,
                        base_vault: state.coin_vault,
                        quote_vault: state.pc_vault,
                        lp_vault: state.lp_mint,
                        open_orders: state.open_orders,
                        target_orders: state.target_orders,

                        // Some market keys will be filled later
                        withdraw_queue: Pubkey::default(),
                        market_version: 3,
                        market_program_id: Pubkey::default(),
                        market_id: state.market,
                        market_authority: Pubkey::default(),
                        market_base_vault: Pubkey::default(),
                        market_quote_vault: Pubkey::default(),
                        market_bids: Pubkey::default(),
                        market_asks: Pubkey::default(),
                        market_event_queue: Pubkey::default(),
                        trade_fee_rate: 0,
                    };

                    // Build the DetectedPool instance
                    let pool_open_time = Utc.timestamp_opt(state.state_data.pool_open_time as i64, 0).unwrap();
                    let pool_info = Pool {
                        keys: pool_keys,
                        in_whitelist: false,
                        slot: 0,
                        initial_price: 0.0,
                        pool_type: PoolType::RaydiumAmm,
                        base_decimals: state.coin_decimals as u8,
                        base_reserves: 0.0, // Not available yet
                        base_supply: 0, // Not available yet
                        base_name: "".to_string(),
                        base_symbol: "".to_string(),
                        base_uri: "".to_string(),
                        base_freezable: false,
                        base_mint_renounced: false,
                        base_meta_mutable: false,
                        base_mint_authority: Default::default(),
                        quote_decimals: state.pc_decimals as u8,
                        quote_reserves: 0.0, // Not available yet
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

    /// A separate subscription method for Raydium AMM pools via Bloxroute.
    /// It uses WebSocket connection to subscribe to the Bloxroute API stream.
    /// Bloxroute is an optional subscription method, if it fails the bot should
    /// continue running with PubSub subscription.
    async fn subscribe_to_amm_pools_bloxroute(&self) {
        if !self.bloxroute_config.enabled {
            warn!("Bloxroute is disabled, skipping Bloxroute subscription...");
            return;
        }

        // If ws url is set, auth token is required
        let ws_url = self.bloxroute_config.ws_url.clone();
        let auth_token = self.bloxroute_config.auth_token.clone();

        if ws_url.is_none() {
            warn!("Bloxroute WebSocket URL not set, skipping Bloxroute subscription...");
            return;
        }
        if auth_token.is_none() {
            error!("Bloxroute WebSocket URL is set, but no auth token provided");
            return;
        }

        let mut attempts = 0;

        loop {
            // Prepare the subscription request
            let subscription_request = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "subscribe",
                "params": ["GetNewRaydiumPoolsStream", { "includeCPMM": false }]
            });

            // Convert the URL into a WebSocket Request
            let mut request = match ws_url.clone().unwrap().to_string().into_client_request() {
                Ok(req) => req,
                Err(e) => {
                    error!("Invalid Bloxroute WebSocket URL: {}", e);
                    break;
                }
            };

            // Add the Authorization header
            request.headers_mut().append(
                "Authorization",
                http::HeaderValue::from_str(auth_token.clone().unwrap().as_str()).unwrap(),
            );

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
                        return;
                    }

                    // Retry the connection
                    continue;
                }
            };

            // Set the start timestamp when the subscription starts
            let start_timestamp = Utc::now();

            // Reset the connection attempts counter
            attempts = 0;

            // Split the WebSocket stream before the select! block
            let (mut write, mut read) = ws_stream.split();

            // Send the subscription request
            if let Err(e) = write.send(Message::Text(subscription_request.to_string())).await {
                error!("Failed to subscribe to Raydium pool stream via Bloxroute: {}", e);
                return;
            }

            tokio::select! {
                // Handle the subscription stream
                _ = async {
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
                                        let pool_keys = PoolKeys {
                                            id: pubkey,
                                            version: pool.liquidity_pool_keys.version,
                                            base_mint: Pubkey::from_str(pool.token1_mint_address.as_str()).unwrap(),
                                            quote_mint: Pubkey::from_str(pool.token2_mint_address.as_str()).unwrap(),
                                            lp_mint: Pubkey::from_str(pool.liquidity_pool_keys.lp_mint.as_str()).unwrap(),
                                            program_id: Pubkey::from_str(pool.liquidity_pool_keys.program_id.as_str()).unwrap(),
                                            authority: Pubkey::from_str(pool.liquidity_pool_keys.authority.as_str()).unwrap(),
                                            base_vault: Pubkey::from_str(pool.liquidity_pool_keys.base_vault.as_str()).unwrap(),
                                            quote_vault: Pubkey::from_str(pool.liquidity_pool_keys.quote_vault.as_str()).unwrap(),
                                            lp_vault: Pubkey::from_str(pool.liquidity_pool_keys.lp_vault.as_str()).unwrap(),
                                            open_orders: Pubkey::from_str(pool.liquidity_pool_keys.open_orders.as_str()).unwrap(),
                                            target_orders: Pubkey::from_str(pool.liquidity_pool_keys.target_orders.as_str()).unwrap(),
                                            withdraw_queue: Pubkey::from_str(pool.liquidity_pool_keys.withdraw_queue.as_str()).unwrap(),
                                            market_version: pool.liquidity_pool_keys.market_version,
                                            market_program_id: Pubkey::from_str(pool.liquidity_pool_keys.market_program_id.as_str()).unwrap(),
                                            market_id: Pubkey::from_str(pool.liquidity_pool_keys.market_id.as_str()).unwrap(),
                                            market_authority: Pubkey::from_str(pool.liquidity_pool_keys.market_authority.as_str()).unwrap(),
                                            market_base_vault: Pubkey::from_str(pool.liquidity_pool_keys.market_base_vault.as_str()).unwrap(),
                                            market_quote_vault: Pubkey::from_str(pool.liquidity_pool_keys.market_quote_vault.as_str()).unwrap(),
                                            market_bids: Pubkey::from_str(pool.liquidity_pool_keys.market_bids.as_str()).unwrap(),
                                            market_asks: Pubkey::from_str(pool.liquidity_pool_keys.market_asks.as_str()).unwrap(),
                                            market_event_queue: Pubkey::from_str(pool.liquidity_pool_keys.market_event_queue.as_str()).unwrap(),
                                            trade_fee_rate: 0,
                                        };
                                        let pool_info = Pool {
                                            keys: pool_keys,
                                            in_whitelist: false,
                                            slot: event.params.result.slot.parse().unwrap_or(0),
                                            pool_type: PoolType::RaydiumAmm,
                                            base_decimals: 0, // Not available yet
                                            base_reserves: pool.token1_reserves.parse().unwrap_or(0.0),
                                            base_supply: 0, // Not available yet
                                            base_name: "".to_string(),
                                            base_symbol: "".to_string(),
                                            base_uri: "".to_string(),
                                            base_freezable: false,
                                            base_mint_renounced: false,
                                            base_meta_mutable: false,
                                            base_mint_authority: Default::default(),
                                            quote_decimals: 0, // Not available yet
                                            quote_reserves: pool.token2_reserves.parse().unwrap_or(0.0),
                                            open_time: Utc.timestamp_opt(pool.open_time.parse().unwrap_or(0), 0).unwrap(),
                                            timestamp: Utc::now(),
                                            initial_price: 0.0,
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
                                               ───────────────────────────────────────\n"
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
                                // Handle the close frame
                                warn!("Bloxroute WebSocket closed: {:?}", frame);
                                break;
                            }
                            _ => {
                                continue;
                            }
                        }
                    }
                } => {}

                // Handle cancellation of the task
                _ = self.cancel_token.cancelled() => {
                    // Close the WebSocket connection and exit the task
                    info!("Unsubscribing from Raydium pool stream via BloxRoute");
                    match write.close().await {
                        Ok(_) => {},
                        Err(e) => error!("Failed to close Bloxroute WebSocket: {}", e),
                    }
                    break;
                }

                // Handle the root unsubscribe
                else => break,
            }

            info!(
                "\n🔄 [RECONNECT]\n\
                Resubscribing to Raydium pool stream via Bloxroute...\n\
                ───────────────────────────\n",
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

async fn try_to_load_initialized_pool(
    rpc_client: Arc<RpcClient>,
    tx_signature: &str,
) -> Result<Option<Pubkey>, Box<dyn Error + Send + Sync>> {
    // Parse the signature
    let signature = match Signature::from_str(tx_signature) {
        Ok(sig) => sig,
        Err(e) => {
            error!("Invalid transaction signature: {}", e);
            return Err(e.into());
        }
    };

    // Fetch the transaction using get_transaction with parsed encoding
    let config = RpcTransactionConfig {
        encoding: Some(UiTransactionEncoding::Base64),
        commitment: Some(CommitmentConfig::finalized()),
        max_supported_transaction_version: Some(0),
    };
    let transaction = match rpc_client.get_transaction_with_config(&signature, config).await {
        Ok(tx) => tx.transaction.transaction,
        Err(e) => {
            error!("Failed to fetch transaction by signature: {}", e);
            return Err(e.into());
        }
    };

    // Parse the transaction data
    match transaction {
        // Json encoded transaction is easier to parse, but slower
        // EncodedTransaction::Json(json) => {
        //     match json.message {
        //         UiMessage::Parsed(message) => {
        //             let instructions = &message.instructions;
        //             println!("{instructions:?}");
        //
        //             Ok(None)
        //         }
        //         _ => {
        //             error!("Unexpected transaction message format");
        //             Err("Unexpected transaction message format".into())
        //         }
        //     }
        // }

        // Binary encoded transaction
        EncodedTransaction::Binary(data_base64, _encoding) => {
            if let Ok(data) = base64::prelude::BASE64_STANDARD.decode(data_base64) {
                // Deserialize the binary data into a Transaction
                if let Ok(tx) = bincode::deserialize::<VersionedTransaction>(&data) {
                    let message = tx.message; // Handle VersionedTransaction
                    let account_keys = message.static_account_keys(); // Get the account keys

                    // Iterate through the instructions
                    for ix in message.instructions() {
                        // Get the program ID
                        let program_id_index = ix.program_id_index as usize;
                        let program_id = match account_keys.get(program_id_index) {
                            Some(pk) => pk,
                            None => continue,
                        };

                        // Skip if not an instruction from the AMM program
                        if *program_id != MainnetProgramId::AmmV4.get_pubkey() {
                            continue;
                        }

                        // Decode the instruction data
                        let instruction_data = &ix.data;

                        // Check if this is the initialize2 instruction
                        if is_initialize2_instruction(instruction_data) {
                            let pool_account_index = ix.accounts[4] as usize;
                            let pool_pubkey = account_keys.get(pool_account_index).unwrap();
                            return Ok(Some(*pool_pubkey));
                        }
                    }

                    // No initialize2 instruction found
                    Ok(None)
                } else {
                    error!("Failed to deserialize transaction data");
                    Err("Failed to deserialize transaction data".into())
                }
            } else {
                error!("Failed to decode Base64 transaction data");
                Err("Failed to decode Base64 transaction data".into())
            }
        }
        _ => {
            error!("Unexpected transaction format");
            Err("Unexpected transaction format".into())
        }
    }
}

// Function to determine if the instruction data corresponds to initialize2
fn is_initialize2_instruction(instruction_data: &[u8]) -> bool {
    const INITIALIZE2_INSTRUCTION_ID: u8 = 1;

    if instruction_data.is_empty() {
        return false;
    }

    instruction_data[0] == INITIALIZE2_INSTRUCTION_ID
}
