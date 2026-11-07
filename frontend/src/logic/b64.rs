//! Base64url decoding for the VAPID public key (Web Push).

fn value(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' | b'-' => 62,
        b'/' | b'_' => 63,
        _ => return None,
    } as u32)
}

/// Accepts standard and URL-safe alphabets, with or without padding.
pub fn decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.trim().bytes().filter(|c| *c != b'=') {
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64url_vapid_keys() {
        assert_eq!(decode("AQID_-8"), Some(vec![1, 2, 3, 255, 239]));
        assert_eq!(decode("AQID/+8="), Some(vec![1, 2, 3, 255, 239]));
        assert_eq!(decode("aGVsbG8"), Some(b"hello".to_vec()));
        assert_eq!(decode(""), Some(vec![]));
        assert_eq!(decode("a$b"), None);
    }
}
