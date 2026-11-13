//! Textual 128-bit store/image identities shared by every binary.

/// Exactly 32 hexadecimal digits, nonzero, big-endian.
pub fn parse(value: &str) -> Result<[u8; 16], String> {
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("ID must contain exactly 32 hexadecimal digits".into());
    }
    let value = u128::from_str_radix(value, 16).map_err(|error| error.to_string())?;
    if value == 0 {
        return Err("ID cannot be zero".into());
    }
    Ok(value.to_be_bytes())
}

/// The canonical directory and report spelling: 32 lowercase hex digits.
pub fn hex(id: [u8; 16]) -> String {
    format!("{:032x}", u128::from_be_bytes(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_hex_round_trip_and_reject_zero_or_short_ids() {
        let id = parse("0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0F").unwrap();
        assert_eq!(id[0], 0x0a);
        assert_eq!(id[15], 0x0f);
        assert_eq!(hex(id), "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0f");
        assert!(parse(&"0".repeat(32)).is_err());
        assert!(parse(&"1".repeat(31)).is_err());
        assert!(parse(&"g".repeat(32)).is_err());
    }
}
