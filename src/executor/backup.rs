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
use crate::executor::order::Order;

/// Backup is a module that periodically saves the trader memory state to disk.
/// It also loads the initial state from disk on startup if it exists.

pub struct Backup {
    // Mutexes for the threadsafe access to the shared state
    pending_orders_file_mutex: Arc<Mutex<()>>,
    pending_orders: Arc<DashMap<Pubkey, Vec<Order>>>, // Reference to the pending orders
    pending_orders_path: String,
    cancel_token: CancellationToken,
}

impl Backup {
    /// Executed orders backup constructor with a reference to the pending orders in Executor module.
    pub fn new(
        pending_orders: Arc<DashMap<Pubkey, Vec<Order>>>,
        cancel_token: CancellationToken,
    ) -> Self {
        // Start a background task to periodically save the executor state to disk
        let path = "./data/pending_orders.json";
        Self {
            cancel_token,
            pending_orders,
            pending_orders_path: path.to_string(),
            pending_orders_file_mutex: Arc::new(Mutex::new(())),
        }
    }

    /// Pending orders auto sync task that periodically saves the pending orders to disk.
    /// It also saves the orders on shutdown.
    pub async fn pending_auto_sync(&self) {
        loop {
            tokio::select! {
                // Periodically save the pending orders to disk
                _ = tokio::time::sleep(Duration::from_secs(8)) => {
                    self.save_pending_orders().await;
                    debug!("Periodic sync of pending orders to disk completed");
                }
                
                // Before shutdown, save the pending orders to disk
                _ = self.cancel_token.cancelled() => {
                    self.save_pending_orders().await;
                    break;
                }
            }
        }
    }

    /// Loads all pending orders from the file and writes them to the shared state.
    pub async fn load_pending_orders(&self) {
        let path = self.pending_orders_path.as_str();
        if path.is_empty() {
            return;
        }

        // Lock the file mutex to prevent concurrent reads
        let _ = self.pending_orders_file_mutex.lock().await;
        info!("Loading pending orders from {}", path);

        // Read the orders from the file
        if Path::new(path).exists() {
            match File::open(path) {
                Ok(mut file) => {
                    let mut data = String::new();
                    if let Err(e) = file.read_to_string(&mut data) {
                        error!("Failed to read {}: {}", path, e);
                        return;
                    }
                    match serde_json::from_str::<HashMap<String, Vec<Order>>>(&data) {
                        Ok(parsed_orders) => {
                            if parsed_orders.len() > 0 {
                                info!("Found {} pending orders", parsed_orders.len());
                            }

                            let orders = DashMap::new();
                            parsed_orders
                                .iter()
                                .for_each(|(pubkey_str, order)| {
                                    match pubkey_str.parse::<Pubkey>() {
                                        Ok(pubkey) => {
                                            orders.insert(pubkey, order.clone());
                                        }
                                        Err(e) => {
                                            error!("Failed to parse pubkey {}: {}", pubkey_str, e);
                                            return;
                                        }
                                    }
                                });

                            // Write the orders to the shared state
                            orders.iter().for_each(|order_ref| {
                                self.pending_orders.insert(*order_ref.key(), order_ref.value().clone());
                            });

                            debug!("Loaded orders from {}", path);
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
            error!("File {} does not exist. Starting with empty orders.", path);
        }
    }

    /// Saves all pending orders to the file. It will replace the existing data.
    pub async fn save_pending_orders(&self) {
        let path = self.pending_orders_path.as_str();
        if path.is_empty() {
            return;
        }

        // Lock the file mutex to prevent concurrent writes
        let _ = self.pending_orders_file_mutex.lock().await;

        // Convert Pubkey to string for serialization to JSON
        let pending_orders = DashMap::new();
        for order_ref in self.pending_orders.iter() {
            pending_orders.insert(order_ref.key().to_string(), order_ref.value().clone());
        }

        // Serialize orders to JSON and write to file
        match serde_json::to_string(&pending_orders) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    error!("Failed to write {}: {}", path, e);
                } else {
                    debug!("Saved orders to {}", path);
                }
            }
            Err(e) => {
                error!("Failed to serialize orders to JSON: {}", e);
            }
        }
    }
}
