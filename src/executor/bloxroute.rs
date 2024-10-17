use base64::Engine;
use log::error;
use reqwest::Client;
use serde_json::json;
use solana_sdk::transaction::Transaction;

/// Send swap details to Bloxroute API and return the unsigned transaction.
pub async fn create_swap_tx(
    serialized_tx: &[u8],
) -> Result<Transaction, Box<dyn std::error::Error + Send + Sync>> {
    let client = Client::new();

    // Return mock transaction for testing
    let mock_tx = Transaction::default();
    return Ok(mock_tx);

    // Bloxroute API endpoint
    let api_url = "https://uk.solana.dex.blxrbdn.com/api/v2/raydium/swap";

    // Prepare the request payload
    let payload = json!({
        "transaction": base64::prelude::BASE64_STANDARD.encode(serialized_tx),
    });

    // Send the POST request
    let response = client
        .post(api_url)
        .json(&payload)
        .send()
        .await?;

    // Check the response
    if response.status().is_success() {
        let response_json: serde_json::Value = response.json().await?;
        // Assuming the response contains the unsigned transaction as a base64 string
        if let Some(tx_str) = response_json
            .get("transactions")
            .and_then(|r| r.get(0))
            .and_then(|r| r.get("content"))
            .and_then(|s| s.as_str()) {
            let tx_bytes = base64::prelude::BASE64_STANDARD.decode(tx_str)?;
            let tx: Transaction = serde_json::from_slice(&tx_bytes)?;
            error!("Swap tx created via Bloxroute.");
            Ok(tx)
        } else {
            Err("Failed to get unsigned transaction from Bloxroute response".into())
        }
    } else {
        let error_text = response.text().await?;
        error!("Unable to submit transaction via Bloxroute: {}", error_text);
        Err("Failed to submit transaction via Bloxroute".into())
    }
}
