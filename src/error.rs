use std::time::Duration;
use tokio::time::sleep;

/// Helper function to handle fetch attempts with exponential backoff.
pub async fn handle_attempt(attempts: &mut u32, max_retries: u32, backoff: u64) -> Result<(), ()> {
    // Increment retry attempt counter
    *attempts += 1;

    // If max retries exceeded, return the error
    if *attempts >= max_retries {
        return Err(());
    }

    // Exponential backoff: Wait for a duration that increases with each retry
    let wait_time = 2u64.pow(*attempts) * backoff; // 100ms, 200ms, 400ms, etc.
    sleep(Duration::from_millis(wait_time)).await;

    Ok(())
}