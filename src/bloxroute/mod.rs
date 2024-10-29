use reqwest::Client;
use serde::{Deserialize, Serialize};
use solana_sdk::{
    transaction::VersionedTransaction,
};
use base64;
use base64::Engine;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SwapRequest {
    owner_address: String,
    in_token: String,
    out_token: String,
    in_amount: f64,
    slippage: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    compute_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compute_price: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tip: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwapResponse {
    transactions: Vec<TransactionMessage>,
    out_amount: f64,
    out_amount_min: f64,
    price_impact: PriceImpactV2,
    fee: Vec<Fee>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransactionMessage {
    content: String, // Base64 encoded transaction
    is_cleanup: bool,
}

#[derive(Deserialize)]
struct PriceImpactV2 {
    percent: f64,
    infinity: String, // "INF_NOT", etc.
}

#[derive(Deserialize)]
struct Fee {
    amount: f64,
    mint: String,
    percent: f64,
}

pub async fn create_swap_amm(
    client: Client,
    api_host: &str,
    auth_header: &str,
    owner_address: &str,
    in_token: &str,
    out_token: &str,
    in_amount: f64,
    slippage: f64,
    compute_limit: Option<u32>,
    compute_price: Option<u64>,
    tip: Option<u64>,
) -> Result<VersionedTransaction, Box<dyn std::error::Error + Send + Sync>> {
    // Build the request body
    let swap_request = SwapRequest {
        in_amount,
        slippage,
        tip,
        compute_limit,
        compute_price,
        owner_address: owner_address.to_string(),
        in_token: in_token.to_string(),
        out_token: out_token.to_string(),
    };

    // API endpoint URL
    let url = format!("{}/api/v2/raydium/swap", api_host);

    // Send the POST request
    let response = client
        .post(url)
        .header("Authorization", auth_header)
        .header("Content-Type", "application/json")
        .json(&swap_request)
        .send()
        .await?;

    // Check if the response status is success
    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await?;
        return Err(format!(
            "HTTP request failed with status {}: {}",
            status, error_text
        ).into());
    }

    // Parse the response JSON
    let swap_response: SwapResponse = response.json().await?;

    // Extract the first transaction from the transactions array
    let tx_message = swap_response
        .transactions
        .get(0)
        .ok_or("No transactions returned in response")?;

    // Decode the base64 transaction content
    let tx_bytes = base64::prelude::BASE64_STANDARD.decode(&tx_message.content)?;

    // Deserialize the transaction bytes into a VersionedTransaction
    let versioned_tx: VersionedTransaction = bincode::deserialize(&tx_bytes)?;

    // Return the transaction
    Ok(versioned_tx)
}
