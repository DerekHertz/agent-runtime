/// Adds two numbers.
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

/// Arithmetic mean of a slice. Returns None for an empty slice.
pub fn mean(xs: &[i64]) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let sum: i64 = xs.iter().sum();
    // BUG (deliberate): off-by-one in the divisor.
    Some(sum as f64 / (xs.len() + 1) as f64)
}

/// Parses "key=value" into a pair. Whitespace around key and value is trimmed.
pub fn parse_kv(s: &str) -> Option<(String, String)> {
    let (k, v) = s.split_once('=')?;
    Some((k.trim().to_string(), v.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_works() {
        assert_eq!(add(2, 3), 5);
    }

    #[test]
    fn mean_works() {
        assert_eq!(mean(&[1, 2, 3]), Some(2.0));
        assert_eq!(mean(&[]), None);
    }

    #[test]
    fn parse_kv_works() {
        assert_eq!(parse_kv(" a = b "), Some(("a".into(), "b".into())));
        assert_eq!(parse_kv("nope"), None);
    }
}
