use solana_transaction_status::{UiTransactionStatusMeta};
use solana_sdk::pubkey::Pubkey;
use solana_transaction_status::option_serializer::OptionSerializer;

/// Get the change in token balance for a given token account in a transaction.
pub fn extract_token_balance_change(
    meta: &UiTransactionStatusMeta,
    owner: &Pubkey,
    token_mint: &Pubkey,
) -> Option<(u64, u64, u64)> {
    // Helper function to extract Option<&T> from OptionSerializer<T>
    fn option_serializer_as_ref<T>(option_serializer: &OptionSerializer<T>) -> Option<&T> {
        match option_serializer {
            OptionSerializer::Some(ref value) => Some(value),
            OptionSerializer::None => None,
            _ => None,
        }
    }

    // Extract pre and post balances
    let pre_balances = match &meta.pre_token_balances {
        OptionSerializer::Some(pre_balances) => pre_balances,
        OptionSerializer::None => &Vec::new(),
        _ => return None,
    };

    let post_balances = match &meta.post_token_balances {
        OptionSerializer::Some(post_balances) => post_balances,
        OptionSerializer::None => &Vec::new(),
        _ => return None,
    };

    // Pre_balance_entry will be None if the account was created in the transaction.
    // Find the entries corresponding to the token account.
    let pre_balance_entry = pre_balances
        .iter()
        .find(|entry| {
            option_serializer_as_ref(&entry.owner) == Some(&owner.to_string()) &&
                &entry.mint == token_mint.to_string().as_str()
        });

    // Post balance should always exist
    let post_balance_entry = match post_balances
        .iter()
        .find(|entry| {
            option_serializer_as_ref(&entry.owner) == Some(&owner.to_string()) &&
                &entry.mint == token_mint.to_string().as_str()
        }) {
        Some(entry) => entry,
        None => return None,
    };

    // Parse pre and post amounts
    let pre_amount = if pre_balance_entry.is_some() {
        match spl_token::try_ui_amount_into_amount(
            pre_balance_entry?.ui_token_amount.real_number_string(),
            pre_balance_entry?.ui_token_amount.decimals,
        ) {
            Ok(amount) => amount,
            Err(_) => return None,
        }
    } else {
        0
    };
    let post_amount = match spl_token::try_ui_amount_into_amount(
        post_balance_entry.ui_token_amount.real_number_string(),
        post_balance_entry.ui_token_amount.decimals,
    ) {
        Ok(amount) => amount,
        Err(_) => return None,
    };

    // Calculate the change
    let amount_change = post_amount - pre_amount;

    Some((pre_amount, post_amount, amount_change))
}
