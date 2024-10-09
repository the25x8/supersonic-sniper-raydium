use std::collections::HashSet;
use std::sync::Arc;
use chrono::DateTime;
use log::{debug, error, info};
use serde::{Deserialize, Serialize};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use tokio::sync::{mpsc, RwLock};
use tokio::sync::mpsc::Sender;

use crate::config::AppConfig;
use crate::raydium;
use crate::solana::quote_mint::{USDC_MINT, WSOL_MINT};

/// The Detector module is responsible for monitoring the Solana blockchain,
/// specifically the Raydium program, to detect new liquidity pools and trading pairs.
/// It uses the Solana RPC client to subscribe to program accounts and filters them
/// based on the watch list provided in the configuration.

/// DetectedPool structure to hold information about the detected pool.
/// It's a high-level representation of the pool data to be sent to the trader.
/// Considering the detector doesn't need to know the exact token reserves or
/// other details, this structure provides only a simplified view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pool {
    pub slot: u64,
    pub pubkey: Pubkey,
    pub status: u64, // Amm initialized status
    pub in_whitelist: bool, // If pool token1 or token2 in watchlist
    pub pool_address: String,
    pub pool_type: PoolType,
    pub base_decimals: Option<u8>, // Only for PubSub events
    pub base_mint: Pubkey,
    pub base_vault: Pubkey,
    pub base_reserves: Option<u128>, // Only for BloxRoute events
    pub quote_decimals: Option<u8>, // Only for PubSub events
    pub quote_mint: Pubkey,
    pub quote_vault: Pubkey,
    pub quote_reserves: Option<u128>, // Only for BloxRoute events
    pub open_time: DateTime<chrono::Utc>,
    pub timestamp: DateTime<chrono::Utc>,
}

/// Enum to represent the type of pool detected.
#[repr(u8)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PoolType {
    RaydiumAmm = 1,
    RaydiumClmm = 2,
}

/// Detected pool is internal structure to hold the detected pool data
/// with meta info such as subscriber type and start time.
#[derive(Debug, Clone)]
pub struct DetectedPool {
    pub data: Pool,
    pub source: SourceType, // Source is a channel where the pool was detected
    pub start_timestamp: DateTime<chrono::Utc>, // Subscription start time
}

#[derive(Debug, Clone, PartialEq)]
#[repr(u8)]
pub enum SourceType {
    PubSub = 1,
    BloxRoute = 2,
}

impl SourceType {
    pub fn to_string(&self) -> String {
        match self {
            SourceType::PubSub => "PubSub".to_string(),
            SourceType::BloxRoute => "BloxRoute".to_string(),
        }
    }
}

pub struct Detector {
    app_config: Arc<AppConfig>,
    client: Arc<RpcClient>,
    watchlist: Arc<Vec<String>>,
    pool_tx: Sender<Pool>, // Bot pool channel
    cache: Arc<RwLock<HashSet<Pubkey>>>,

    // Internal channels
    tx: Sender<DetectedPool>,
    rx: mpsc::Receiver<DetectedPool>,
}

impl Detector {
    /// Creates a new instance of the Detector module.
    ///
    /// # Arguments
    ///
    /// * `rpc_ws_url` - A string slice that holds the URL of the Solana RPC WebSocket endpoint.
    /// * `client` - A reference to the Solana RPC client.
    /// * `tx` - A sender channel to send detected pools to the trader.
    /// * `watch_list` - A vector of Pubkey addresses to filter the detected pools.
    ///
    /// # Returns
    ///
    /// * `Detector` - A new instance of the Detector module.
    pub fn new(config: Arc<AppConfig>, client: Arc<RpcClient>, tx: Sender<Pool>) -> Self {
        // Build the watch list of tokens, converting strings to Pubkey
        let watch_list_arc = Arc::new(config.detector.watchlist.clone());

        // Create the internal detected pool channel
        let (internal_tx, internal_rx) = mpsc::channel::<DetectedPool>(10);

        // Initialize the hashset to store detected pools in memory
        let pool_cache = Arc::new(RwLock::new(HashSet::new()));

        Self {
            client,
            pool_tx: tx,
            tx: internal_tx,
            rx: internal_rx,
            cache: pool_cache,
            app_config: config.clone(),
            watchlist: watch_list_arc,
        }
    }

    /// Starts the Detector module.
    ///
    /// # Returns
    ///
    /// * `()` - The function runs indefinitely until the application is terminated.
    pub async fn start(self) {
        // Clone `pool_rx` to allow moving it into the async block
        let pool_rx = self.rx;
        let pool_tx = self.tx.clone(); // Clone tx as well for usage inside async block

        // Initialize the Raydium detector
        let raydium_detector = raydium::detector::RaydiumDetector::new(
            pool_tx,
            self.app_config.rpc.ws_url.as_str(),
            self.app_config.bloxroute.ws_url.as_deref(),
            self.app_config.bloxroute.auth_token.as_deref(),
        ).await;

        // Start all internal tasks
        tokio::join!(
            // Start the pool receiver task
            Self::start_pool_receiver(pool_rx, self.pool_tx, self.cache, self.watchlist),

            // Start the Raydium detector
            async move {
                raydium_detector.start().await; // Start the Raydium detector
                
                // If Raydium detector fails, the application will panic,
                // because the bot can't operate without the detector.
                panic!("Detector tasks stopped unexpectedly");
            },
        );
    }

    /// Starts the pool receiver task that handles incoming detected pools
    /// from all detector modules. It forwards the pools to the bot pool channel.
    async fn start_pool_receiver(mut pool_rx: mpsc::Receiver<DetectedPool>, tx: Sender<Pool>, cache: Arc<RwLock<HashSet<Pubkey>>>, watchlist: Arc<Vec<String>>) {
        if let Err(e) = tokio::spawn(async move {
            while let Some(detected_pool) = pool_rx.recv().await {
                let mut pool = detected_pool.data;

                // Check pool status, allow only pools with status 4-7
                // if pool.status < 4 || pool.status > 7 {
                //     continue;
                // }

                // Check if the pool is already detected, and skip already detected pools.
                if Self::check_pool_cache(cache.clone(), &pool.pubkey).await {
                    continue;
                }

                // If watchlist is not empty, check if the
                // base or quote token is in the watchlist.
                if !watchlist.is_empty() {
                    let in_watchlist = Self::check_watchlist(
                        watchlist.clone(),
                        &pool.base_mint.to_string(),
                        &pool.quote_mint.to_string(),
                    );

                    // Skip the pool if both tokens are not in the watchlist
                    if !in_watchlist {
                        continue;
                    }

                    // Mark the pool as in the watchlist
                    pool.in_whitelist = true;
                } else {
                    // If watchlist is empty and source is PubSub, we should check
                    // the open time of the pool to avoid processing old pools.
                    if detected_pool.source == SourceType::PubSub {
                        if !Self::is_open_time_valid(
                            pool.open_time.timestamp(),
                            detected_pool.start_timestamp.timestamp(),
                        ) {
                            continue;
                        }
                    }
                }

                // In some pools base and quote are reversed, quote should always be USDC/WSOL.
                // Otherwise, we need to swap the values to avoid confusion.
                if pool.base_mint.to_string() == USDC_MINT ||
                    pool.base_mint.to_string() == WSOL_MINT {
                    let temp_mint = pool.base_mint;
                    let temp_vault = pool.base_vault;
                    let temp_reserves = pool.base_reserves;
                    let temp_decimals = pool.base_decimals;

                    // Swap the values
                    pool.base_mint = pool.quote_mint;
                    pool.base_vault = pool.quote_vault;
                    pool.base_reserves = pool.quote_reserves;
                    pool.base_decimals = pool.quote_decimals;

                    pool.quote_mint = temp_mint;
                    pool.quote_vault = temp_vault;
                    pool.quote_reserves = temp_reserves;
                    pool.quote_decimals = temp_decimals;
                }

                info!(
                    "\n🌊 [POOL DETECTED] New Raydium Pool Identified!\n\
                    ───────────────────────────\n\
                    🏦 Pool Address:     {}\n\
                    🌍 Source:           {}\n\
                    ⏳ Open Time:        {}\n\
                    ───────────────────────────\n",
                    pool.pubkey.to_string(),
                    detected_pool.source.to_string(),
                    pool.open_time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                );


                // Send the detected pool data to the bot pool channel
                if let Err(e) = tx.send(pool).await {
                    error!("Failed to send detected pool to bot pool channel: {:?}", e);
                }
            }
        }).await {
            error!("Pool receiver task failed: {:?}", e);
        }
    }

    /// Checks if the pool is already detected and adds it to the cache.
    /// Returns `true` if the pool is already detected, `false` otherwise.
    async fn check_pool_cache(cache: Arc<RwLock<HashSet<Pubkey>>>, pubkey: &Pubkey) -> bool {
        {
            let pool_cache = cache.read().await;
            if pool_cache.contains(pubkey) {
                return true;
            }
        }

        // Add the new pool to the cache
        let mut pool_cache = cache.write().await;
        pool_cache.insert(*pubkey);
        false
    }

    /// Only process pools that were opened after the subscription started.
    /// Handles the case where the pool_open_time is set to zero.
    fn is_open_time_valid(open_time: i64, start_timestamp: i64) -> bool {
        if open_time > 0 && open_time < start_timestamp {
            debug!("Skipping pool opened before subscription started");
            return false;
        } else if open_time == 0 {
            debug!("Skipping pool with zero open time");
            return false;
        }
        true
    }

    /// Check token of pair in watchlist. If watchlist is empty, always return true.
    /// Otherwise, return true if at least one token of pair in watchlist.
    fn check_watchlist(watch_list: Arc<Vec<String>>, base_mint: &str, quote_mint: &str) -> bool {
        if watch_list.contains(&base_mint.to_string()) ||
            watch_list.contains(&quote_mint.to_string()) {
            return true;
        }

        debug!("Pool token mint not in watchlist, skipping...");
        false
    }
}
