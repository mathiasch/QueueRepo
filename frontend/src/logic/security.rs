//! WP11: presentation logic of MFA, sessions, admin console, audit log and
//! runner fleet – pure functions, tested natively.
use serde_json::Value;

/// QR code of `text` as one SVG path (`d`) on a `size × size` grid of
/// modules (quiet zone of 4 modules included). `None` if it does not fit.
pub fn qr_path(text: &str) -> Option<(usize, String)> {
    let code =
        qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).ok()?;
    let w = code.width();
    let colors = code.to_colors();
    let quiet = 4;
    let mut d = String::new();
    for y in 0..w {
        let mut x = 0;
        while x < w {
            if colors[y * w + x] == qrcode::Color::Dark {
                // Runs of dark modules as one rectangle.
                let start = x;
                while x < w && colors[y * w + x] == qrcode::Color::Dark {
                    x += 1;
                }
                d.push_str(&format!(
                    "M{} {}h{}v1h-{}z",
                    start + quiet,
                    y + quiet,
                    x - start,
                    x - start
                ));
            } else {
                x += 1;
            }
        }
    }
    Some((w + 2 * quiet, d))
}

/// The base32 seed in groups of four for manual entry.
pub fn group_secret(s: &str) -> String {
    s.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Removes `null` members recursively: browsers reject `null` for optional
/// WebAuthn dictionary members.
pub fn strip_nulls(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.retain(|_, x| !x.is_null());
            for x in m.values_mut() {
                strip_nulls(x);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(strip_nulls),
        _ => {}
    }
}

/// Base64url without padding (WebAuthn JSON of webauthn-rs).
pub fn b64url(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len() * 4 / 3 + 3);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        let len = c.len() + 1;
        for i in 0..len {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// Paths of binary members in WebAuthn options (base64url in JSON,
/// `ArrayBuffer` for the browser).
pub const BINARY_OPTION_FIELDS: [&str; 2] = ["challenge", "user.id"];
pub const BINARY_LIST_FIELDS: [&str; 2] = ["excludeCredentials", "allowCredentials"];

/// Duration since `secs` seconds ago in words ("vor 3 Min.").
pub fn ago(secs: i64) -> String {
    match secs {
        s if s < 60 => "gerade eben".into(),
        s if s < 3600 => format!("vor {} Min.", s / 60),
        s if s < 86_400 => format!("vor {} Std.", s / 3600),
        s => format!("vor {} Tagen", s / 86_400),
    }
}

/// Money of the quota row.
pub fn usd(v: f64) -> String {
    format!("{v:.2} $").replace('.', ",")
}

/// Text field → optional number (empty = no limit); `Err` with the text.
pub fn parse_limit<T: std::str::FromStr>(s: &str) -> Result<Option<T>, String> {
    let t = s.trim().replace(',', ".");
    if t.is_empty() {
        return Ok(None);
    }
    t.parse()
        .map(Some)
        .map_err(|_| format!("„{}“ ist keine Zahl.", s.trim()))
}

/// Readable details of an audit entry (keys sorted, objects compact).
pub fn audit_details(v: &Value) -> String {
    match v {
        Value::Object(m) if !m.is_empty() => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            keys.into_iter()
                .map(|k| {
                    let val = match &m[k] {
                        Value::String(s) => s.clone(),
                        Value::Null => "–".into(),
                        other => other.to_string(),
                    };
                    format!("{k}: {val}")
                })
                .collect::<Vec<_>>()
                .join(" · ")
        }
        _ => String::new(),
    }
}

/// Env snippet for a newly registered runner (token shown once).
pub fn runner_env(token: &str) -> String {
    format!(
        "RUNNER_TOKEN={token}\nRUNNER_BIND=0.0.0.0:8090\n# Diese Datei nur für den Runner-Benutzer lesbar machen (chmod 600)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_code_has_modules_and_quiet_zone() {
        let (size, d) =
            qr_path("otpauth://totp/Queue:a@b?secret=ABCDEFGHIJKLMNOP&issuer=Queue").unwrap();
        assert!(size >= 29 + 8, "version ≥ 3 plus quiet zone: {size}");
        assert!(
            d.starts_with("M4 4h7v1h-7z"),
            "finder pattern top-left: {}",
            &d[..20]
        );
        assert!(d.matches('M').count() > 50);
        assert!(
            qr_path(&"x".repeat(5000)).is_none(),
            "too long for a QR code"
        );
    }

    #[test]
    fn secrets_and_numbers() {
        assert_eq!(group_secret("ABCDEFGHIJ"), "ABCD EFGH IJ");
        assert_eq!(usd(3.5), "3,50 $");
        assert_eq!(parse_limit::<f64>(" 2,5 "), Ok(Some(2.5)));
        assert_eq!(parse_limit::<i32>(""), Ok(None));
        assert!(parse_limit::<i32>("x").is_err());
        assert_eq!(ago(30), "gerade eben");
        assert_eq!(ago(7200), "vor 2 Std.");
        assert!(runner_env("qr_x").contains("RUNNER_TOKEN=qr_x"));
    }

    #[test]
    fn webauthn_json_helpers() {
        let mut v: Value =
            serde_json::json!({"a": null, "b": {"c": null, "d": 1}, "e": [{"f": null}]});
        strip_nulls(&mut v);
        assert_eq!(v, serde_json::json!({"b": {"d": 1}, "e": [{}]}));
        // RFC 4648 §10 vectors, URL-safe alphabet, no padding.
        assert_eq!(b64url(b""), "");
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foo"), "Zm9v");
        assert_eq!(b64url(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
        assert_eq!(
            crate::logic::b64::decode(&b64url(&[1, 2, 3, 250])).unwrap(),
            vec![1, 2, 3, 250]
        );
    }

    #[test]
    fn audit_detail_text() {
        let v = serde_json::json!({"z": 1, "a": "x", "n": null});
        assert_eq!(audit_details(&v), "a: x · n: – · z: 1");
        assert_eq!(audit_details(&serde_json::json!({})), "");
    }
}
