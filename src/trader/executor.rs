use std::sync::Arc;
use std::time::Duration;
use chrono::Utc;
use dashmap::DashMap;
use futures::StreamExt;
use solana_client::nonblocking::rpc_client::RpcClient;
use log::{debug, error, info, warn};
use solana_program::pubkey::Pubkey;
use tokio::sync::mpsc::{Sender};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tokio_util::time::DelayQueue;
use crate::trader::backup::Backup;
use crate::wallet::Wallet;
use crate::trader::models::{Order, OrderStatus};

// Maximum retry attempts for sending transactions to the blockchain
const MAX_TX_RETRIES: u32 = 5;

pub struct Executor {
    client: Arc<RpcClient>,
    wallet: Arc<Wallet>,

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
        let backup = Arc::new(Backup::new_for_pending_orders(
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
                            tokio::spawn(async move {
                                match Self::execute_order(order, executed_orders_tx_clone, pending_orders_clone).await {
                                    Ok(_) => (),
                                    Err(e) => warn!("Failed to execute order: {}", e),
                                }
                            });
                        }

                        // Sleep to avoid busy loop
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                } => {}
            }
        });

        Self {
            client,
            wallet,
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
            📊 Order {}!\n\
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

        // If the order has a delay, send it to the delay queue
        if order.delay > 0 {
            match self.delay_queue_tx.send(order).await {
                Ok(_) => (),
                Err(e) => {
                    info!("Failed to send order to delay queue: {}", e);
                    return Err(Box::new(e));
                }
            }
            return Ok(());
        }

        // Otherwise, execute the order immediately in a separate task to not block the main loop
        let executed_orders_tx = self.executed_orders_tx.clone();
        let pending_orders = self.pending_orders.clone();
        tokio::spawn(async move {
            match Self::execute_order(order, executed_orders_tx, pending_orders).await {
                Ok(_) => (),
                Err(e) => {
                    error!("Failed to execute order: {}", e);
                }
            }
        });

        Ok(())
    }

    /// Executes the order and returns the updated order
    async fn execute_order(
        mut order: Order,
        executed_orders_tx: Sender<Order>,
        pending_orders: Arc<DashMap<Pubkey, Vec<Order>>>,
    ) -> Result<Order, Box<dyn std::error::Error>> {
        // Simulate tx confirmation and receipt time (~800ms)
        tokio::time::sleep(Duration::from_millis(800)).await;

        // Update order status
        order.tx_id = Some("dummy_tx_id".to_string());
        order.signature = Some("dummy_signature".to_string());
        order.status = OrderStatus::Completed;
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
            order.signature.as_ref().unwrap(),
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
            drop(orders_ref); // Explicitly drop the mutable reference

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