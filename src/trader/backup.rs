use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use dashmap::DashMap;
use log::{debug, error, info, warn};
use solana_sdk::pubkey::Pubkey;
use tokio::sync::Mutex;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use crate::trader::Trade;

/// Backup is a module that periodically saves the trader memory state to disk.
/// It also loads the initial state from disk on startup if it exists.

pub struct Backup {
    // Mutexes for the threadsafe access to the shared state
    active_trades_file_mutex: Arc<Mutex<()>>,
    trades_history_file_mutex: Arc<Mutex<()>>,
    active_trades: Arc<DashMap<Pubkey, Vec<Trade>>>, // Reference to the trades history
    active_trades_path: String,
    trades_history_path: String,

    // Cancellation token for sync data on shutdown
    cancel_token: CancellationToken,
}

impl Backup {
    /// Trades backup constructor with a reference to the trades in Trader module.
    pub fn new(
        active_trades: Arc<DashMap<Pubkey, Vec<Trade>>>,
        cancel_token: CancellationToken,
    ) -> Self {
        // Start a background task to periodically save the trader state to disk
        let path = "./data/active_trades.json";
        Self {
            cancel_token,
            active_trades,
            active_trades_path: path.to_string(),
            trades_history_path: "./data/trades_history.json".to_string(),
            active_trades_file_mutex: Arc::new(Mutex::new(())),
            trades_history_file_mutex: Arc::new(Mutex::new(())),
        }
    }

    /// Trades auto sync task that periodically saves the active trades to disk.
    /// It also saves the trades on shutdown.
    pub async fn trades_auto_sync(&self) {
        loop {
            tokio::select! {
                // Periodically save the active trades to disk
                _ = tokio::time::sleep(Duration::from_secs(8)) => {
                    self.save_active_trades().await;
                    debug!("Periodic sync of active trades to disk");
                }

                // Before shutdown, save the active trades to disk
                _ = self.cancel_token.cancelled() => {
                    self.save_active_trades().await;
                    break;
                }
            }
        }
    }

    pub async fn load_active_trades(&self) {
        let path = self.active_trades_path.as_str();
        if path.is_empty() {
            warn!("Active trades file path is empty. Skipping load.");
            return;
        }

        // Lock the file mutex to prevent concurrent reads
        let _ = self.active_trades_file_mutex.lock().await;
        info!("Loading active trades from {}", path);

        if Path::new(path).exists() {
            match File::open(path) {
                Ok(mut file) => {
                    let mut data = String::new();
                    if let Err(e) = file.read_to_string(&mut data) {
                        error!("Failed to read {}: {}", path, e);
                        return;
                    }
                    match serde_json::from_str::<HashMap<String, Vec<Trade>>>(&data) {
                        Ok(parsed_trades) => {
                            if parsed_trades.len() > 0 {
                                info!("Found {} active trades", parsed_trades.len());
                            }

                            // Convert pubkeys from strings to Pubkey
                            let converted_trades = DashMap::new();
                            for (pubkey_str, trades) in parsed_trades.iter() {
                                match pubkey_str.parse::<Pubkey>() {
                                    Ok(pubkey) => {
                                        converted_trades.insert(pubkey, trades.clone());
                                    }
                                    Err(e) => {
                                        error!("Failed to parse pubkey {}: {}", pubkey_str, e);
                                    }
                                }
                            }

                            // Write the trades to the shared state
                            converted_trades.iter().for_each(|trade_ref| {
                                self.active_trades.insert(*trade_ref.key(), trade_ref.value().clone());
                            });
                        }
                        Err(e) => {
                            error!("Failed to parse {}: {}", path, e);
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to open {}: {}", path, e);
                }
            }
        } else {
            error!("File {} does not exist. Starting with empty trades.", path);
        }
    }

    async fn save_active_trades(&self) {
        let path = self.active_trades_path.as_str();
        if path.is_empty() {
            warn!("Active trades file path is empty. Skipping save.");
            return;
        }

        // Lock the file mutex to prevent concurrent writes
        let _ = self.active_trades_file_mutex.lock().await;

        // Convert Pubkey to string for serialization to JSON
        let converted_trades = DashMap::new();
        for trade_ref in self.active_trades.iter() {
            converted_trades.insert(trade_ref.key().to_string(), trade_ref.value().clone());
        }

        // Serialize trades to JSON and write to file
        match serde_json::to_string(&converted_trades) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    error!("Failed to write {}: {}", path, e);
                } else {
                    debug!("Saved active trades to {}", path);
                }
            }
            Err(e) => {
                error!("Failed to serialize active trades to JSON: {}", e);
            }
        }
    }

    pub async fn load_trades_history(&self, trades_history: Arc<Mutex<Vec<Trade>>>) {
        let path = self.trades_history_path.as_str();
        if path.is_empty() {
            warn!("Trades history file path is empty. Skipping load.");
            return;
        }

        // Lock the file mutex to prevent concurrent reads
        let _ = self.trades_history_file_mutex.lock().await;

        if Path::new(path).exists() {
            match File::open(path) {
                Ok(mut file) => {
                    let mut data = String::new();
                    if let Err(e) = file.read_to_string(&mut data) {
                        error!("Failed to read {}: {}", path, e);
                        return;
                    }
                    match serde_json::from_str::<Vec<Trade>>(&data) {
                        Ok(parsed_trades) => {
                            let mut trades = trades_history.lock().await;
                            *trades = parsed_trades;
                            debug!("Loaded trades history from {}", path);
                        }
                        Err(e) => {
                            error!("Failed to parse {}: {}", path, e);
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to open {}: {}", path, e);
                }
            }
        } else {
            error!("File {} does not exist. Starting with empty trades history.", path);
        }
    }

    /// Parses the trades history from the file, deserializes the JSON and appends the new trade.
    /// The updated list is then serialized back to JSON and written to the file.
    pub async fn save_trade_in_history(&self, trade: Trade) {
        // Lock the file mutex to prevent concurrent writes
        let _ = self.trades_history_file_mutex.lock().await;

        let path = self.trades_history_path.as_str();
        let mut trades = Vec::new();

        if Path::new(path).exists() {
            match File::open(path) {
                Ok(mut file) => {
                    let mut data = String::new();
                    if let Err(e) = file.read_to_string(&mut data) {
                        error!("Failed to read {}: {}", path, e);
                        return;
                    }
                    match serde_json::from_str::<Vec<Trade>>(&data) {
                        Ok(parsed_trades) => {
                            trades = parsed_trades;
                        }
                        Err(e) => {
                            error!("Failed to parse {}: {}", path, e);
                            return;
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to open {}: {}", path, e);
                    return;
                }
            }
        } else {
            error!("File {} does not exist. Starting with empty trades history.", path);
        }

        // Append the new trade
        trades.push(trade);

        // Serialize trades to JSON and write to file
        match serde_json::to_string(&trades) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    error!("Failed to write {}: {}", path, e);
                } else {
                    debug!("Saved trades history to {}", path);
                }
            }
            Err(e) => {
                error!("Failed to serialize trades history to JSON: {}", e);
            }
        }
    }
}
