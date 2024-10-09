use std::str::FromStr;

pub fn token_amount_to_float(amount: &str, decimals: u8) -> f64 {
    f64::from_str(amount).unwrap_or(0.0) /
        10f64.powi(decimals as i32)
}

pub fn float_to_token_amount(amount: f64, decimals: u8) -> String {
    format!("{:.0}", amount * 10f64.powi(decimals as i32))
}