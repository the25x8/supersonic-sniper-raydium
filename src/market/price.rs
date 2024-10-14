use solana_account_decoder::parse_token::UiTokenAmount;
use solana_program::pubkey::Pubkey;
use crate::solana::quote_mint::USDC_MINT;

/// Converts the reserves to f64 and calculates the price.
/// Base and quote reserves should be adjusted for token decimals before calling this function.
/// The base token is the token being bought, and the quote token is the token being sold.
/// WSOL and USDC are treated as quote tokens, but sometimes the base token is WSOL or USDC.
/// In that case, the tokens and reserves are swapped before calculating the price. 
/// The price is the amount of quote token needed to buy one base token.
pub fn convert_reserves_to_price(
    base_mint: Pubkey,
    quote_mint: Pubkey,
    base_reserves: &UiTokenAmount,
    quote_reserves: &UiTokenAmount,
) -> Result<f64, Box<dyn std::error::Error + Send + Sync>> {
    // Convert base and quote reserves into f64, adjusting for the token decimals
    let base_reserve: f64 = base_reserves.amount.parse::<f64>()? / 10f64.powi(base_reserves.decimals as i32);
    let quote_reserve: f64 = quote_reserves.amount.parse::<f64>()? / 10f64.powi(quote_reserves.decimals as i32);

    // Calculate price using the correct reserve amounts
    let price = adjust_tokens_and_calculate_price(base_mint, quote_mint, base_reserve, quote_reserve);

    Ok(price)
}

pub fn adjust_tokens_and_calculate_price(
    mut base_mint: Pubkey,
    mut quote_mint: Pubkey,
    mut base_reserves: f64,
    mut quote_reserves: f64,
) -> f64 {
    // Check if base token is WSOL or USDC
    if base_mint == spl_token::native_mint::id() || base_mint.to_string() == USDC_MINT {
        // Swap base and quote tokens and reserves
        std::mem::swap(&mut base_mint, &mut quote_mint);
        std::mem::swap(&mut base_reserves, &mut quote_reserves);
    }

    // Now, quote token is WSOL/USDC
    // Calculate the price
    

    quote_reserves / base_reserves
}