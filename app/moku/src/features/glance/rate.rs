//! The exchange-rate chip: Frankfurter (the European Central Bank's reference rates, free, no
//! key), `https://api.frankfurter.dev/v1/latest?base=USD&symbols=IDR`. Pure; the tests run on a
//! live sample saved in `fixtures/`.
//!
//! The ECB publishes once per working day (about 16:00 CET), so asking every 6 hours is plenty.

pub const BASE_URL: &str = "https://api.frankfurter.dev/v1/latest";

#[derive(Clone, Debug, PartialEq)]
pub struct Rate {
    pub base: String,
    pub quote: String,
    /// What one unit of `base` is worth in `quote`.
    pub value: f64,
}

/// The request for a pair (both are 3 upper-case letters: the config validates them).
pub fn url(base: &str, quote: &str) -> String {
    format!("{BASE_URL}?base={base}&symbols={quote}")
}

/// Read a response for the pair that was asked. `None` when the rate is missing, zero, negative
/// or not a number, or when the answer is for another base.
pub fn parse(json: &str, base: &str, quote: &str) -> Option<Rate> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if v.get("base").and_then(|b| b.as_str()).is_some_and(|b| !b.eq_ignore_ascii_case(base)) {
        return None;
    }
    let value = v.get("rates")?.get(quote)?.as_f64().filter(|x| x.is_finite() && *x > 0.0)?;
    Some(Rate { base: base.to_string(), quote: quote.to_string(), value })
}

/// `18.229` (Indonesian) / `18,229` (English): thousands grouped; 0 decimals from 1,000 up, 2
/// from 10, 4 below.
pub fn format_value(v: f64, english: bool) -> String {
    let (decimals, group) = if v >= 1000.0 { (0, true) } else if v >= 10.0 { (2, false) } else { (4, false) };
    let s = format!("{v:.decimals$}");
    let (int, frac) = s.split_once('.').map_or((s.as_str(), ""), |(i, f)| (i, f));
    let (thousands, decimal) = if english { (',', '.') } else { ('.', ',') };
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if group && i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(thousands);
        }
        grouped.push(c);
    }
    if frac.is_empty() { grouped } else { format!("{grouped}{decimal}{frac}") }
}

/// The chip's text, as quoted on an exchange: `USD/IDR 18.229` (what 1 USD is worth in IDR), `EUR/USD 1,0823`.
pub fn chip_text(r: &Rate, english: bool) -> String {
    format!("{}/{} {}", r.base, r.quote, format_value(r.value, english))
}

#[cfg(test)]
mod tests {
    use super::*;

    // One live sample (2026-11-24).
    const SAMPLE: &str = include_str!("fixtures/frankfurter.json");

    #[test]
    fn the_live_sample_parses() {
        let r = parse(SAMPLE, "USD", "IDR").expect("the live sample");
        assert_eq!((r.base.as_str(), r.quote.as_str(), r.value), ("USD", "IDR", 18229.0));
        assert_eq!(chip_text(&r, false), "USD/IDR 18.229");
        assert_eq!(chip_text(&r, true), "USD/IDR 18,229");
    }

    #[test]
    fn the_request() {
        assert_eq!(url("USD", "IDR"), "https://api.frankfurter.dev/v1/latest?base=USD&symbols=IDR");
        assert_eq!(url("EUR", "JPY"), "https://api.frankfurter.dev/v1/latest?base=EUR&symbols=JPY");
    }

    #[test]
    fn other_pairs_and_bad_answers() {
        let eur = r#"{"amount":1.0,"base":"EUR","date":"2026-11-24","rates":{"USD":1.0823}}"#;
        let r = parse(eur, "EUR", "USD").unwrap();
        assert_eq!(chip_text(&r, false), "EUR/USD 1,0823");
        assert_eq!(chip_text(&r, true), "EUR/USD 1.0823");
        // The pair that was not asked for, a missing rate, junk.
        assert_eq!(parse(eur, "USD", "USD"), None, "answer for another base");
        assert_eq!(parse(eur, "EUR", "IDR"), None, "quote missing");
        for bad in ["", "{}", "<html>", r#"{"rates":{"IDR":0}}"#, r#"{"rates":{"IDR":-5}}"#, r#"{"rates":{"IDR":"x"}}"#, r#"{"rates":{"IDR":null}}"#, r#"{"message":"not found"}"#] {
            assert_eq!(parse(bad, "USD", "IDR"), None, "{bad:?}");
        }
        // A response without "base" is still read.
        assert_eq!(parse(r#"{"rates":{"IDR":16000.5}}"#, "USD", "IDR").map(|r| r.value), Some(16000.5));
    }

    #[test]
    fn numbers_are_grouped_and_rounded_for_the_language() {
        assert_eq!(format_value(18229.0, false), "18.229");
        assert_eq!(format_value(18229.4, false), "18.229");
        assert_eq!(format_value(1234567.0, true), "1,234,567");
        assert_eq!(format_value(1000.0, false), "1.000");
        assert_eq!(format_value(999.999, false), "1000,00", "rounding up past 1000 is not grouped (two decimals below 1000)");
        assert_eq!(format_value(155.4321, false), "155,43");
        assert_eq!(format_value(155.4321, true), "155.43");
        assert_eq!(format_value(1.0823, false), "1,0823");
        assert_eq!(format_value(0.0065, true), "0.0065");
        assert_eq!(format_value(12.0, true), "12.00");
    }
}
