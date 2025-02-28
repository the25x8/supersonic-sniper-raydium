use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_account_decoder::{UiAccountData, UiAccountEncoding};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::sync::broadcast::{Receiver, Sender};
use log::{debug, error, info, warn};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use base64::Engine;
use borsh::BorshDeserialize;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use futures::stream::SplitSink;
use serde::{Deserialize, Serialize};
use serde_json::json;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_filter::RpcFilterType::DataSize;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;
use tungstenite::client::IntoClientRequest;
use tungstenite::Message;
use crate::error::handle_attempt;
use crate::{market, raydium};
use crate::config::BloxrouteConfig;
use crate::detector::{PoolKeys};

const PRICE_CACHE_TTL: u64 = 1000; // Price cache time-to-live in milliseconds

// Maximum retry attempts for fetching data from the blockchain
const MAX_SUBSCRIPTION_RETRIES: u32 = 5;
const RECONNECT_BACKOFF: u64 = 1000; // Reconnection backoff in milliseconds

#[derive(Debug, Clone)]
pub struct MarketData {
    pub pool: Pubkey,
    pub price: f64,
    pub base_reserves: f64,
    pub quote_reserves: f64,
    pub timestamp: std::time::Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BXReservesEvent {
    Reserves(BXNewReservesEvent),
    WsConnected(BXConnectedEvent),
    WsError(BXErrorEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BXConnectedEvent {
    pub id: i64,
    pub jsonrpc: String,
    pub result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BXErrorEvent {
    pub id: i64,
    pub jsonrpc: String,
    pub error: BXError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BXError {
    pub code: i64,
    pub message: String,
    pub data: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BXNewReservesEvent {
    pub jsonrpc: String,
    pub method: String,
    pub params: BXReservesParams,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BXReservesParams {
    pub subscription: String,
    pub result: BXReservesResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BXReservesResult {
    pub slot: String,
    pub reserves: BXPoolReserves,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BXPoolReserves {
    pub project: String,
    pub pool_address: String,
    pub token1_address: String,
    pub token1_reserves: String,
    pub token2_address: String,
    pub token2_reserves: String,
}

#[derive(Debug, Clone)]
pub struct PoolMeta {
    pub keys: PoolKeys,
    pub base_decimals: u8,
    pub quote_decimals: u8,
}

pub struct MarketMonitor {
    market_tx: Sender<MarketData>,
    rpc_client: Arc<RpcClient>,
    pubsub_client: Arc<PubsubClient>,
    watchlist: Arc<DashMap<Pubkey, PoolMeta>>,
    watchlist_add_tx: Sender<(Pubkey, PoolMeta)>,
    watchlist_remove_tx: Sender<Pubkey>,
    prices: Arc<DashMap<Pubkey, (f64, std::time::Instant)>>,
    bloxroute_config: BloxrouteConfig,

    // For cancelling the market monitor task when the bot is stopped
    cancel_token: CancellationToken,
}

impl MarketMonitor {
    pub async fn new(
        rpc_client: Arc<RpcClient>,
        rpc_ws_url: &str,
        market_tx: Sender<MarketData>,
        bloxroute_config: &BloxrouteConfig,
        cancel_token: CancellationToken,
    ) -> Self {
        // Initialize the Pubsub client instance
        let pubsub_client = match PubsubClient::new(rpc_ws_url).await {
            Ok(client) => client,
            Err(e) => {
                cancel_token.cancel(); // Gracefully exit the bot
                panic!("Failed to create WebSocket client: {:?}", e);
            }
        };

        let prices = Arc::new(DashMap::new());
        let watchlist = Arc::new(DashMap::new());
        let pubsub_client = Arc::new(pubsub_client);

        let (watchlist_add_tx, watchlist_add_rx) = tokio::sync::broadcast::channel(50);
        let (watchlist_remove_tx, watchlist_remove_rx) = tokio::sync::broadcast::channel(50);

        // Run the watchlist add and remove tasks
        let watchlist_clone = watchlist.clone();
        tokio::spawn(async move {
            Self::watchlist_add_loop(watchlist_add_rx, watchlist_clone).await;
        });

        let watchlist_clone = watchlist.clone();
        tokio::spawn(async move {
            Self::watchlist_remove_loop(watchlist_remove_rx, watchlist_clone).await;
        });

        Self {
            prices,
            watchlist,
            market_tx,
            rpc_client,
            pubsub_client,
            cancel_token,
            watchlist_add_tx,
            watchlist_remove_tx,
            bloxroute_config: bloxroute_config.clone(),
        }
    }

    pub async fn start(&self) {
        // Run the main market monitor task
        let market_data_tx = self.market_tx.clone();
        let rpc_client = self.rpc_client.clone();
        let pubsub_client = self.pubsub_client.clone();
        let watchlist_add_rx = self.watchlist_add_tx.subscribe();
        let watchlist_remove_rx = self.watchlist_remove_tx.subscribe();
        let watchlist = self.watchlist.clone();
        let cancel_token = self.cancel_token.clone();
        let prices_map = self.prices.clone();

        // If Bloxroute is enabled, run the Bloxroute subscription task
        // if self.bloxroute_config.enabled {
        //     self.subscribe_to_amm_reserves_bloxroute(
        //         watchlist_add_rx,
        //         watchlist_remove_rx,
        //         watchlist,
        //     ).await;
        //     return;
        // }

        // Otherwise, run the standard PubSub subscription task
        Self::subscribe_to_amm_reserves_pubsub(
            rpc_client,
            pubsub_client,
            market_data_tx,
            watchlist,
            prices_map,
            cancel_token,
        ).await;
    }

    /// Run the watchlist add loop to add pools to the watchlist,
    /// it also fetches the initial price of the pool and emits it to the market_tx channel.
    async fn watchlist_add_loop(
        mut watchlist_add_rx: Receiver<(Pubkey, PoolMeta)>,
        watchlist: Arc<DashMap<Pubkey, PoolMeta>>,
    ) {
        while let Ok((pubkey, pool_keys)) = watchlist_add_rx.recv().await {
            // // Add the pool to the watchlist if it doesn't exist
            let exists = watchlist.contains_key(&pubkey);
            if !exists {
                watchlist.insert(pubkey, pool_keys);
                info!("🔍 Pool {} added to watchlist", pubkey);
                // watchlist lock is dropped here
            } else {
                // Pool already exists in the watchlist, skip
                continue;
            }

            // Get liquidity state for the pool
            // let state = match raydium::get_amm_liquidity_state(rpc_client.clone(), &pubkey).await {
            //     Ok(state) => state,
            //     Err(e) => {
            //         error!("Failed to fetch AMM liquidity state: {}", e);
            //         continue;
            //     }
            // };
            // 
            // // Fetch the initial price of the pool and emit to the market_tx channel
            // let (base_reserves, quote_reserves) = match raydium::get_amm_pool_reserves(
            //     rpc_client.clone(),
            //     &state.coin_vault,
            //     &state.pc_vault,
            // ).await {
            //     Ok(reserves) => (reserves[0].clone(), reserves[1].clone()),
            //     Err(e) => {
            //         error!("Failed to fetch initial AMM pool reserves: {}", e);
            //         continue;
            //     }
            // };
            // 
            // // Calculate the initial price of the pool
            // let price = match market::price::convert_reserves_to_price(
            //     state.coin_vault_mint,
            //     state.pc_vault_mint,
            //     &base_reserves,
            //     &quote_reserves,
            // ) {
            //     Ok(price) => price,
            //     Err(e) => {
            //         error!("Failed to calculate initial token price: {}", e);
            //         continue;
            //     }
            // };
            // 
            // // Emit the initial market data to the market_tx channel
            // let base_reserves_amount = token_amount_to_float(&base_reserves.amount, base_reserves.decimals);
            // let quote_reserves_amount = token_amount_to_float(&quote_reserves.amount, quote_reserves.decimals);
            // match market_tx.send(MarketData {
            //     price,
            //     pool: pubkey,
            //     base_reserves: base_reserves_amount,
            //     quote_reserves: quote_reserves_amount,
            //     timestamp: std::time::Instant::now(),
            // }).await {
            //     Ok(_) => (),
            //     Err(e) => {
            //         error!("Failed to send initial market data: {}", e);
            //         continue;
            //     }
            // }
        }
    }

    /// Loop to remove pools from the watchlist when requested.
    async fn watchlist_remove_loop(
        mut watchlist_remove_rx: Receiver<Pubkey>,
        watchlist: Arc<DashMap<Pubkey, PoolMeta>>,
    ) {
        while let Ok(pubkey) = watchlist_remove_rx.recv().await {
            // Remove the pool from the watchlist
            let removed = watchlist.remove(&pubkey);
            if removed.is_some() {
                info!("🔍 Pool {} removed from watchlist", pubkey);
            }
        }
    }

    /// The task subscribes to the AMM program, listens for liquidity updates,
    /// and emits updated market data to the market_tx channel.
    async fn subscribe_to_amm_reserves_pubsub(
        rpc_client: Arc<RpcClient>,
        pubsub: Arc<PubsubClient>,
        market_tx: Sender<MarketData>,
        watchlist: Arc<DashMap<Pubkey, PoolMeta>>,
        prices: Arc<DashMap<Pubkey, (f64, std::time::Instant)>>,
        cancel_token: CancellationToken,
    ) {
        // Clone the Pubsub client instance
        let mut attempts = 0;

        loop {
            // Combine the custom filters with common filters
            let filters = vec![
                DataSize(752), // Filter by data size (752 bytes for Raydium LiquidityStateV4)

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

            // Subscribe to the AMM program
            let (mut accounts, program_unsubscribe) =
                match pubsub.program_subscribe(
                    &raydium::MainnetProgramId::AmmV4.get_pubkey(),
                    Some(config),
                ).await {
                    Ok(subscription) => subscription,
                    Err(e) => {
                        error!("Failed to subscribe to AMM program: {}", e);

                        // Try to reconnect, and return if max retries exceeded
                        if let Err(_) = handle_attempt(
                            &mut attempts,
                            MAX_SUBSCRIPTION_RETRIES,
                            RECONNECT_BACKOFF,
                        ).await {
                            error!("Max subscription retries for Raydium pool stream in Market Monitor exceeded");
                            break;
                        }

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
                        🌊 Subscribed to Raydium Reserves Stream via PubSub\n\
                        ▶️ Waiting for new reserves...\n\
                        ───────────────────────────────────────\n"
                    );

                    // Process account updates
                    while let Some(response) = accounts.next().await {
                        // Read public key of amm pool
                        let pubkey = match Pubkey::from_str(&response.value.pubkey) {
                            Ok(pk) => pk,
                            Err(e) => {
                                error!("Failed to parse pool pubkey: {}", e);
                                continue;
                            }
                        };

                        // Handle the account update in a separate task
                        let rpc_client = rpc_client.clone();
                        let market_tx = market_tx.clone();
                        let prices = prices.clone();
                        let watchlist = watchlist.clone();
                        tokio::spawn(async move {
                            if let Err(e) = Self::handle_account_update(
                                pubkey,
                                &response.value.account.data,
                                rpc_client.clone(),
                                prices.clone(),
                                watchlist.clone(),
                                market_tx.clone(),
                            ).await {
                                error!("Failed to handle account update: {}", e);
                            }
                        });
                    }
                } => {}

                // Unsubscribe from the program subscription and exit the task if cancelled
                _ = cancel_token.cancelled() => {
                    info!("Unsubscribing from Raydium reserves stream in Market Monitor...");
                    program_unsubscribe().await;
                    break;
                }

                else => break,
            }

            // Wait before retrying to avoid busy looping
            info!(
                "\n🔄 [RECONNECT]\n\
                Resubscribing to Raydium reserves stream via PubSub...\n\
                ───────────────────────────\n",
            );

            // Try to reconnect after a delay
            if let Err(_) = handle_attempt(
                &mut attempts,
                MAX_SUBSCRIPTION_RETRIES,
                RECONNECT_BACKOFF,
            ).await {
                error!("Max reconnection retries for Raydium reserves stream exceeded");
                break;
            }
        }

        // Cancel the token
        info!("Market monitor has been terminated");
        cancel_token.cancel();
    }

    /// The task subscribes to the AMM program via Bloxroute, listens for reserves updates,
    /// and emits updated market data to the market_tx channel.
    async fn subscribe_to_amm_reserves_bloxroute(
        &self,
        mut watchlist_add_rx: Receiver<(Pubkey, PoolMeta)>,
        mut watchlist_remove_rx: Receiver<Pubkey>,
        watchlist: Arc<DashMap<Pubkey, PoolMeta>>,
    ) {
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

            // Reset the connection attempts counter
            attempts = 0;

            // Split the WebSocket stream before the select! block
            let (mut write, mut read) = ws_stream.split();

            // Subscribe to the watchlist initial pools
            if let Err(e) = self.subscribe_to_watchlist_pools(&watchlist, &mut write).await {
                error!("Failed to update Bloxroute subscription: {}", e);
            }

            loop {
                tokio::select! {
                    // Listen new pools to add to the watchlist
                    Ok(_) = watchlist_add_rx.recv() => {
                         // Update the subscription with the new watchlist
                        if let Err(e) = self.subscribe_to_watchlist_pools(&watchlist, &mut write).await {
                            error!("Failed to add pool to Bloxroute subscription: {}", e);
                        }
                    }

                    // Listen for removal of pools from the watchlist
                    Ok(_) = watchlist_remove_rx.recv() => {
                        // Update the subscription with the new watchlist
                        if let Err(e) = self.subscribe_to_watchlist_pools(&watchlist, &mut write).await {
                            error!("Failed to remove pool from Bloxroute subscription: {}", e);
                        }
                    }

                    // Handle the subscription events
                    Some(message_result) = read.next() => {
                        let message = match message_result {
                            Ok(msg) => msg,
                            Err(e) => {
                                error!("Failed to receive message from Bloxroute WebSocket: {}", e);
                                continue;
                            }
                        };

                        // Process the message
                        match message {
                            Message::Text(text) => {
                                // Attempt to deserialize into BXPoolEvent
                                let event = match serde_json::from_str::<BXReservesEvent>(&text) {
                                    Ok(event) => event,
                                    Err(e) => {
                                        error!("Failed to parse Raydium reserves event: {}", e);
                                        continue;
                                    }
                                };

                                // Process the event
                                match event {
                                    BXReservesEvent::Reserves(event) => {
                                        // Handle the reserves event in separate task
                                        let market_tx = self.market_tx.clone();
                                        let prices = self.prices.clone();
                                        let watchlist = self.watchlist.clone();
                                        tokio::spawn(async move {
                                            if let Err(e) = Self::handle_reserves_event(
                                                &event.params.result.reserves,
                                                prices.clone(),
                                                watchlist.clone(),
                                                market_tx.clone(),
                                            ).await {
                                                error!("Failed to handle reserves event: {}", e);
                                            }
                                        });
                                    }
                                    BXReservesEvent::WsConnected(_) => {
                                        info!(
                                            "\n\
                                            🌊 Subscribed to Raydium Reserves Stream via BloxRoute\n\
                                            ▶️ Waiting for new reserves...\n\
                                            ───────────────────────────────────────\n"
                                        );
                                    }
                                    BXReservesEvent::WsError(event) => {
                                        error!("Bloxroute WebSocket error: {:?}", event.error);
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

                    // Handle cancellation of the task
                    _ = self.cancel_token.cancelled() => {
                        // Close the WebSocket connection and exit the task
                        info!("Unsubscribing from Raydium reserves stream via BloxRoute");
                        match write.close().await {
                            Ok(_) => {},
                            Err(e) => error!("Failed to close Bloxroute WebSocket: {}", e),
                        }
                        return;
                    }

                    // Handle the root unsubscribe
                    else => break,
                }
            }

            info!(
                "\n🔄 [RECONNECT]\n\
                Resubscribing to Raydium reserves stream via Bloxroute...\n\
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

    /// Send a subscription request to Bloxroute with the updated watchlist
    /// to receive updates for the specified pools.
    async fn subscribe_to_watchlist_pools(
        &self,
        watchlist: &DashMap<Pubkey, PoolMeta>,
        write: &mut SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let pools = watchlist.iter()
            .map(|r| r.key().to_string())
            .collect::<Vec<String>>();

        // Prepare the subscription request
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "subscribe",
            "params": [
                "GetPoolReservesStream", {
                    "pools": pools
                }
            ]
        });

        // Send the subscription request
        if let Err(e) = write.send(Message::Text(request.to_string())).await {
            error!("Failed to subscribe to Raydium reserves stream via Bloxroute: {}", e);
            return Err(e.into());
        }

        Ok(())
    }

    /// Add a pool to the watchlist
    pub fn add_to_watchlist(&self, pubkey: &Pubkey, pool_meta: PoolMeta) {
        if let Err(e) = self.watchlist_add_tx.send((*pubkey, pool_meta)) {
            error!("Failed to add pool to watchlist: {}", e);
        }
    }

    /// Remove a pool from the watchlist
    pub fn remove_from_watchlist(&self, pubkey: &Pubkey) {
        if let Err(e) = self.watchlist_remove_tx.send(*pubkey) {
            error!("Failed to remove pool from watchlist: {}", e);
        }
    }

    /// This method handles new reserves updates from Bloxroute
    /// and emits market data to the market_tx channel.
    async fn handle_reserves_event(
        reserves: &BXPoolReserves,
        prices: Arc<DashMap<Pubkey, (f64, std::time::Instant)>>,
        watchlist: Arc<DashMap<Pubkey, PoolMeta>>,
        market_tx: Sender<MarketData>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let pool_pubkey = Pubkey::from_str(&reserves.pool_address)?;

        // Get pool data from the watchlist
        let pool = match watchlist.get(&pool_pubkey) {
            Some(pool) => pool.value().clone(),
            None => return Ok(())
        };

        // Calculate the price of the base token in terms of the quote token
        let token1_reserves = reserves.token1_reserves.parse::<f64>()?;
        let token2_reserves = reserves.token2_reserves.parse::<f64>()?;

        // Adjust the reserves for token decimals
        let base_reserves = token1_reserves / 10f64.powi(pool.base_decimals as i32);
        let quote_reserves = token2_reserves / 10f64.powi(pool.quote_decimals as i32);

        // Calculate price using the correct reserve amounts
        let price = market::price::adjust_tokens_and_calculate_price(
            pool.keys.base_mint,
            pool.keys.quote_mint,
            base_reserves,
            quote_reserves,
        );

        // Liquidity rug pull check
        // let liquidity_check = match raydium::check_liquidity(
        //     &state.lp_mint,
        //     state.lp_amount as f64,
        //     rpc_client,
        // ).await {
        //     Ok(liquidity_check) => liquidity_check,
        //     Err(e) => {
        //         error!("Failed to check liquidity: {}", e);
        //         return Err(e);
        //     }
        // };

        debug!(
            "Market data updated for pool {}\n\
            Base reserves: {}\n\
            Quote reserves: {}\n\
            Price: {}\n\
            Liquidity check: N/A\n",
            pool_pubkey,
            base_reserves,
            quote_reserves,
            price,
            //liquidity_check
        );

        // Send market data to the market_tx channel
        match market_tx.send(MarketData {
            price, // New price of base token in terms of quote token
            pool: pool_pubkey,
            base_reserves: token1_reserves,
            quote_reserves: token2_reserves,
            timestamp: std::time::Instant::now(),
        }) {
            Ok(_) => (),
            Err(e) => {
                error!("Failed to send market data: {}", e);
                return Err(e.into());
            }
        }

        // Update the cache with the new price and timestamp
        prices.insert(pool_pubkey, (price, std::time::Instant::now()));

        Ok(())
    }

    /// Handle account deserializes the liquidity state for AMM pools
    /// and emits market data to the market_tx channel.
    async fn handle_account_update(
        pubkey: Pubkey,
        data: &UiAccountData,
        rpc_client: Arc<RpcClient>,
        prices: Arc<DashMap<Pubkey, (f64, std::time::Instant)>>,
        watchlist: Arc<DashMap<Pubkey, PoolMeta>>,
        market_tx: Sender<MarketData>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match data {
            UiAccountData::Binary(data_base64, _encoding) => {
                if let Ok(data) = base64::prelude::BASE64_STANDARD.decode(data_base64) {
                    // Parse the liquidity state data
                    let state = match raydium::LiquidityStateV4::try_from_slice(&data) {
                        Ok(state) => state,
                        Err(e) => {
                            error!("Failed to parse liquidity state v4 data: {}", e);
                            return Err(e.into());
                        }
                    };

                    // Check if the pool is in the watchlist
                    if !watchlist.contains_key(&pubkey) {
                        return Ok(());
                    }

                    // If price is cached, skip fetching
                    if let Some(entry) = prices.get(&pubkey) {
                        // If the price hasn't changed in the last 5 seconds, skip fetching
                        if entry.value().1.elapsed() < Duration::from_millis(PRICE_CACHE_TTL) {
                            return Ok(());
                        }
                    }

                    // Fetch the reserves of the AMM pool if cache is stale or empty
                    let (base_reserves, quote_reserves) =
                        match raydium::get_amm_pool_reserves(
                            rpc_client.clone(),
                            &state.coin_vault,
                            &state.pc_vault,
                        ).await {
                            Ok(reserves) => (reserves[0].clone(), reserves[1].clone()),
                            Err(e) => {
                                error!("Failed to fetch AMM pool reserves: {}", e);
                                return Err(e);
                            }
                        };

                    // Calculate the price of the base token in terms of the quote token
                    let price = match market::price::convert_reserves_to_price(
                        state.coin_vault_mint,
                        state.pc_vault_mint,
                        &base_reserves,
                        &quote_reserves,
                    ) {
                        Ok(price) => price,
                        Err(e) => {
                            error!("Failed to calculate token price: {}", e);
                            return Err(e);
                        }
                    };

                    // Liquidity rug pull check
                    let liquidity_check = match raydium::check_liquidity(
                        &state.lp_mint,
                        state.lp_amount as f64,
                        rpc_client,
                    ).await {
                        Ok(liquidity_check) => liquidity_check,
                        Err(e) => {
                            error!("Failed to check liquidity: {}", e);
                            return Err(e);
                        }
                    };

                    debug!(
                        "Market data updated for pool {}\n\
                        Base reserves: {}\n\
                        Quote reserves: {}\n\
                        Price: {}\n\
                        Liquidity check: {:?}\n",
                        pubkey,
                        base_reserves.ui_amount_string.to_string(),
                        quote_reserves.ui_amount_string.to_string(),
                        price,
                        liquidity_check
                    );

                    // Send market data to the market_tx channel
                    match market_tx.send(MarketData {
                        price, // New price of base token in terms of quote token
                        pool: pubkey,
                        base_reserves: f64::from_str(&base_reserves.amount)?,
                        quote_reserves: f64::from_str(&quote_reserves.amount)?,
                        timestamp: std::time::Instant::now(),
                    }) {
                        Ok(_) => (),
                        Err(e) => {
                            error!("Failed to send market data: {}", e);
                            return Err(e.into());
                        }
                    }

                    // Update the cache with the new price and timestamp
                    prices.insert(pubkey, (price, std::time::Instant::now()));
                }

                Ok(())
            }
            _ => {
                error!("Unexpected data format for account {}", pubkey);
                Err("Unexpected data format".into())
            }
        }
    }
}