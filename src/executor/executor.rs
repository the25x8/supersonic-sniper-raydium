use std::sync::Arc;
use std::time::Duration;
use chrono::Utc;
use dashmap::DashMap;
use futures::StreamExt;
use solana_client::nonblocking::rpc_client::RpcClient;
use log::{error, info, warn};
use solana_program::pubkey::Pubkey;
use tokio::sync::mpsc::Sender;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tokio_util::time::DelayQueue;
use crate::config::ExecutorType;
use crate::executor::order::{Order, OrderDirection, OrderStatus};
use crate::executor::backup::Backup;
use crate::executor::{bloxroute, swap_tx};
use crate::wallet::Wallet;

// Maximum retry attempts for sending transactions to the blockchain
const MAX_TX_RETRIES: u32 = 5;

pub struct Executor {
    // Delay queue for scheduling orders
    delay_queue_tx: Sender<Order>,

    // Executed orders dashmap
    pending_orders: Arc<DashMap<Pubkey, Vec<Order>>>,

    // Executed orders channel
    executed_orders_tx: Sender<Order>,
}

impl Executor {
    pub async fn new(
        client: Arc<RpcClient>,
        wallet: Arc<Wallet>,
        executed_orders_tx: Sender<Order>,
        cancel_token: CancellationToken,
    ) -> Self {
        let (delay_queue_tx, mut delay_queue_rx) = tokio::sync::mpsc::channel::<Order>(10);

        // Initialize the backup helper and load the pending orders from disk
        let pending_orders: Arc<DashMap<Pubkey, Vec<Order>>> = Arc::new(DashMap::new());
        let backup = Arc::new(Backup::new(
            pending_orders.clone(),
            cancel_token.clone(),
        ));
        backup.load_pending_orders().await;

        // Start auto-saving the pending orders to disk every 5 seconds
        let backup_clone = backup.clone();
        tokio::spawn(async move {
            backup_clone.pending_auto_sync().await;
        });

        // Start the executor task
        let executed_orders_tx_clone = executed_orders_tx.clone();
        let pending_orders_clone = pending_orders.clone();
        tokio::spawn(async move {
            let delay_queue = Arc::new(RwLock::new(DelayQueue::new()));

            // Send the initial pending orders to the delay queue
            for orders in pending_orders_clone.iter() {
                // Clone the orders to avoid borrowing issues
                for order in orders.clone().into_iter() {
                    let now = Utc::now().timestamp_millis() as u64;
                    // If delayed_at is set, calculate the delay
                    let delay_ms = if order.delayed_at > now {
                        order.delayed_at - now
                    } else {
                        // Otherwise, use the default
                        order.delay
                    };
                    let delay = Duration::from_millis(delay_ms);
                    delay_queue.write().await.insert(order, delay);
                }
            }

            let delay_queue_clone = delay_queue.clone();
            let rpc_client_clone = client.clone();
            tokio::select! {
                // Receive new orders to be added to the delay queue
                _ = async {
                    while let Some(order) = delay_queue_rx.recv().await {
                        let now = Utc::now().timestamp_millis() as u64;
                        // If delayed_at is set, calculate the delay
                        let delay_ms = if order.delayed_at > now {
                            order.delayed_at - now
                        } else {
                            // Otherwise, use the default
                            order.delay
                        };
                        let delay = Duration::from_millis(delay_ms);
                        delay_queue_clone.write().await.insert(order, delay);
                    }
                } => {}

                // Process expired items from the delay queue
                _ = async {
                    loop {
                        // Get copy of the delay queue and drop the lock
                        while let Some(expired) = delay_queue.write().await.next().await {
                            let order = expired.into_inner();
                            let executed_orders_tx_clone = executed_orders_tx_clone.clone();
                            let pending_orders_clone = pending_orders_clone.clone();
                            let rpc_client_clone = rpc_client_clone.clone();
                            let wallet = wallet.clone();
                            tokio::spawn(async move {
                                match Self::execute_order(
                                    rpc_client_clone,
                                    order,
                                    wallet,
                                    executed_orders_tx_clone,
                                    pending_orders_clone,
                                ).await {
                                    Ok(_) => (),
                                    Err(e) => warn!("Failed to execute order: {}", e),
                                }
                            });
                        }

                        // Sleep to avoid busy loop
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                } => {}
            }
        });

        Self {
            pending_orders,
            delay_queue_tx,
            executed_orders_tx,
        }
    }

    /// Order method to send orders to pending_orders channel,
    /// which will be scheduled by the executor based on the delay
    /// provided by the strategy for the order's side (buy/sell).
    pub async fn order(&self, order: Order) -> Result<(), Box<dyn std::error::Error>> {
        // If the pool already exists, append the order to the vector of orders for that pool
        let order_clone = order.clone();
        if let Some(mut orders_ref) = self.pending_orders.get_mut(&order.pool) {
            orders_ref.push(order_clone);
        } else {
            // Otherwise, create a new pool with the order
            self.pending_orders.insert(order.pool, vec![order_clone]);
        }

        info!(
            "\n\
            📊 Order {}\n\
            Pool: {}\n\
            Trade: {}\n\
            Side: {:?}\n\
            Amount in: {}\n\
            Created at: {}\n",
            if order.delay > 0 {
                "scheduled with delay"
            } else {
                "will be executed immediately"
            },
            order.pool.to_string(),
            order.trade_id,
            order.direction,
            order.amount_in,
            order.created_at
        );

        // Either send the order to the delay queue or execute it immediately
        // we will send the order to the delay queue for execution.
        match self.delay_queue_tx.send(order).await {
            Ok(_) => (),
            Err(e) => {
                info!("Failed to send order to delay queue: {}", e);
                return Err(Box::new(e));
            }
        }

        Ok(())
    }

    /// Method to execute an order. It creates all the necessary transactions
    /// with instructions and sends them to the blockchain for execution.
    /// Tx will be sent via rpc client or BloxRoute API with bridge.
    /// The method blocks until the receipt of the transaction is received.
    /// Retries attempts are made in case of failure. If the order is executed
    /// successfully, it will be sent to the executed_orders_tx channel.
    async fn execute_order(
        rpc_client: Arc<RpcClient>,
        mut order: Order,
        wallet: Arc<Wallet>,
        executed_orders_tx: Sender<Order>,
        pending_orders: Arc<DashMap<Pubkey, Vec<Order>>>,
    ) -> Result<Order, Box<dyn std::error::Error>> {
        let wallet_clone = wallet.clone();

        // Choose submission method: via RPC or Bloxroute API
        let mut tx = if order.executor == ExecutorType::Bloxroute {
            // In case of bloxroute, the tx is generated via the Bloxroute API.
            // The tx should be signed and submitted to the blockchain by RPC client.
            match bloxroute::create_swap_tx(&vec![]).await {
                Ok(tx) => tx,
                Err(e) => {
                    error!("Failed to create swap tx via Bloxroute: {}", e);
                    return Err(e);
                }
            }
        } else if order.executor == ExecutorType::RPC {
            // Otherwise, build swap tx (Create ATA, SwapIn) manually.
            let rpc_client_clone = rpc_client.clone();

            // Based on the order direction, build the appropriate transaction:
            match order.direction {
                // Build swap in tx
                OrderDirection::BaseIn => {
                    match swap_tx::build_swap_in_tx(rpc_client_clone, &wallet_clone.keypair, &order).await {
                        Ok(tx) => tx,
                        Err(e) => {
                            error!("Failed to build swap in transaction for order: {}", e);
                            return Err(e);
                        }
                    }
                }

                // Build swap out tx
                OrderDirection::BaseOut => {
                    match swap_tx::build_swap_out_tx(rpc_client_clone, &wallet_clone.keypair, &order).await {
                        Ok(tx) => tx,
                        Err(e) => {
                            error!("Failed to build swap out transaction for order: {}", e);
                            return Err(e);
                        }
                    }
                }
            }
        } else {
            return Err("Invalid executor type".into());
        };

        // Get the latest blockhash
        let rpc_client_clone = rpc_client.clone();
        let recent_blockhash = match rpc_client_clone.get_latest_blockhash().await {
            Ok(hash) => hash,
            Err(err) => {
                return Err(Box::new(err));
            }
        };

        // Sign the transaction with the wallet keypair
        tx.sign(&[&wallet.keypair], recent_blockhash);

        // Send tx to the blockchain, wait for confirmation receipt.
        // let signature = match rpc_client.send_and_confirm_transaction(&tx).await {
        //     Ok(signature) => signature,
        //     Err(e) => {
        //         error!("Failed to send and confirm transaction: {}", e);
        //         return Err(Box::new(e));
        //     }
        // };

        // Simulate tx confirmation and receipt time (~1600ms)
        tokio::time::sleep(Duration::from_millis(1600)).await;
        let signature = "simulated_signature";

        // Update order status
        order.tx_id = Some(signature.to_string());
        order.status = OrderStatus::Completed;

        // It's not accurate to use current_timestamp() here.
        // We need to get slot and timestamp from the transaction receipt.
        // For simplicity, we will use the current timestamp.
        order.confirmed_at = current_timestamp();

        info!(
            "\n✅ Order Executed!\n\
             ─────────────────────────────────────────────────────\n\
             ▶️ Trade ID:        {}\n\
             ▶️ Pool:            {}\n\
             ▶️ Tx:              {}\n\
             ▶️ Status:          {}\n\
             ▶️ Side:            {:?}\n\
             ▶️ Amount In:       {:.10}\n\
             ▶️ Confirmed At:    {}\n",
            order.trade_id,
            order.pool.to_string(),
            order.tx_id.as_ref().unwrap(),
            order.status,
            order.direction,
            order.amount_in,
            order.confirmed_at
        );

        // Emit the executed order to the executed_orders_tx channel
        match executed_orders_tx.send(order.clone()).await {
            Ok(_) => (),
            Err(e) => {
                info!("Failed to send executed order: {}", e);
                return Err(Box::new(e));
            }
        }

        // Remove the order from the pending orders hashmap
        if let Some(mut orders_ref) = pending_orders.get_mut(&order.pool) {
            // Retain all orders except the one that was executed
            orders_ref.retain(|o| o.trade_id != order.trade_id);

            // After modifying the entry, check if the entry is empty
            let pool_is_empty = orders_ref.is_empty();
            drop(orders_ref); // Drop the mutable reference before removing the entry

            if pool_is_empty {
                pending_orders.remove(&order.pool);
            }
        }

        Ok(order)
    }
}

fn current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}