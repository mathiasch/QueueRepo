//! WP11: second factors – TOTP (RFC 6238 on HMAC-SHA-1, RFC 4226
//! truncation), recovery codes, WebAuthn (webauthn-rs), short-lived auth
//! flows and the brute-force throttle.
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use queued_api::security::{RECOVERY_ALPHABET, RECOVERY_CODES, RECOVERY_CODE_LEN};
use rand::Rng;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;
use webauthn_rs::prelude::{Passkey, Url, Webauthn, WebauthnBuilder};

use crate::crypto::sha256_hex;

pub const TOTP_DIGITS: u32 = 6;
pub const TOTP_PERIOD: i64 = 30;
/// Accepted clock drift in time steps (±30 s).
pub const TOTP_WINDOW: i64 = 1;
/// Seed length (RFC 4226 recommends 160 bit).
pub const TOTP_SECRET_BYTES: usize = 20;

/// Second login step: lifetime and wrong answers before the flow ends.
pub const FLOW_MINUTES: i64 = 5;
pub const FLOW_MAX_ATTEMPTS: i32 = 5;

/// Throttle: failures within the window before the key is locked.
pub const LOGIN_MAX_FAILURES: i32 = 10;
pub const MFA_MAX_FAILURES: i32 = 5;
pub const THROTTLE_WINDOW_MIN: i64 = 15;
pub const LOCK_MIN: i64 = 15;

const B32: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32 without padding.
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            out.push(B32[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(B32[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = vec![];
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.chars().filter(|c| *c != '=' && !c.is_whitespace()) {
        let v = B32
            .iter()
            .position(|b| *b as char == c.to_ascii_uppercase())? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            out.push((buf >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    Some(out)
}

/// HOTP value (RFC 4226) for counter `step`.
pub fn hotp(secret: &[u8], step: u64, digits: u32) -> u32 {
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret).expect("any key length");
    mac.update(&step.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = (h[h.len() - 1] & 0x0f) as usize;
    let bin = (u32::from(h[off] & 0x7f) << 24)
        | (u32::from(h[off + 1]) << 16)
        | (u32::from(h[off + 2]) << 8)
        | u32::from(h[off + 3]);
    bin % 10u32.pow(digits)
}

pub fn step_at(unix: i64) -> i64 {
    unix.div_euclid(TOTP_PERIOD)
}

/// Time step whose code matches `code` within the window around `unix`
/// (newest first). The caller stores it and refuses steps ≤ it (replay).
pub fn totp_match(secret: &[u8], code: &str, unix: i64) -> Option<i64> {
    let code: u32 = code.parse().ok()?;
    let now = step_at(unix);
    let mut found = None;
    // Every candidate is computed (no early exit on a match).
    for d in -TOTP_WINDOW..=TOTP_WINDOW {
        let step = now + d;
        if step < 0 {
            continue;
        }
        if hotp(secret, step as u64, TOTP_DIGITS) == code {
            found = Some(found.map_or(step, |f: i64| f.max(step)));
        }
    }
    found
}

pub fn new_totp_secret() -> Vec<u8> {
    let mut b = vec![0u8; TOTP_SECRET_BYTES];
    rand::thread_rng().fill(&mut b[..]);
    b
}

fn uri_escape(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

/// `otpauth://` URI for authenticator apps (Key Uri Format).
pub fn totp_uri(issuer: &str, account: &str, secret_b32: &str) -> String {
    format!(
        "otpauth://totp/{}:{}?secret={secret_b32}&issuer={}&algorithm=SHA1&digits={TOTP_DIGITS}&period={TOTP_PERIOD}",
        uri_escape(issuer),
        uri_escape(account),
        uri_escape(issuer)
    )
}

/// A fresh set of recovery codes (canonical form, without dash).
pub fn new_recovery_codes() -> Vec<String> {
    let mut rng = rand::thread_rng();
    (0..RECOVERY_CODES)
        .map(|_| {
            (0..RECOVERY_CODE_LEN)
                .map(|_| RECOVERY_ALPHABET[rng.gen_range(0..RECOVERY_ALPHABET.len())] as char)
                .collect()
        })
        .collect()
}

/// HMAC-SHA-256 of a canonical recovery code with a pepper derived from
/// `MASTER_KEY`: a database dump alone does not allow offline guessing.
pub fn recovery_hash(master_key: &[u8; 32], code: &str) -> String {
    let mut k = Hmac::<sha2::Sha256>::new_from_slice(master_key).expect("key");
    k.update(b"queue-recovery-code-pepper-v1");
    let pepper = k.finalize().into_bytes();
    let mut m = Hmac::<sha2::Sha256>::new_from_slice(&pepper).expect("key");
    m.update(code.as_bytes());
    m.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Relying party of this instance (id = host of `PUBLIC_URL`).
pub fn webauthn(public_url: &str) -> Result<Webauthn> {
    let origin = Url::parse(public_url)?;
    let host = origin
        .host_str()
        .ok_or_else(|| anyhow!("PUBLIC_URL has no host"))?
        .to_string();
    Ok(WebauthnBuilder::new(&host, &origin)?
        .rp_name("Queue")
        .build()?)
}

pub async fn passkeys(db: &PgPool, user: Uuid) -> Result<Vec<(Uuid, Passkey)>> {
    let rows: Vec<(Uuid, Value)> = sqlx::query_as(
        "SELECT id, passkey FROM webauthn_credentials WHERE user_id=$1 ORDER BY created_at",
    )
    .bind(user)
    .fetch_all(db)
    .await?;
    rows.into_iter()
        .map(|(id, v)| Ok((id, serde_json::from_value(v)?)))
        .collect()
}

/// Second factors the person has set up.
pub async fn methods(
    db: &PgPool,
    user: Uuid,
) -> sqlx::Result<Vec<queued_api::security::MfaMethod>> {
    use queued_api::security::MfaMethod;
    let (totp, passkeys, recovery): (bool, bool, bool) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM mfa_totp WHERE user_id=$1 AND confirmed_at IS NOT NULL),
                EXISTS (SELECT 1 FROM webauthn_credentials WHERE user_id=$1),
                EXISTS (SELECT 1 FROM recovery_codes WHERE user_id=$1 AND used_at IS NULL)",
    )
    .bind(user)
    .fetch_one(db)
    .await?;
    let mut m = vec![];
    if totp {
        m.push(MfaMethod::Totp);
    }
    if passkeys {
        m.push(MfaMethod::Webauthn);
    }
    if (totp || passkeys) && recovery {
        m.push(MfaMethod::Recovery);
    }
    Ok(m)
}

pub async fn has_mfa(db: &PgPool, user: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM mfa_totp WHERE user_id=$1 AND confirmed_at IS NOT NULL)
             OR EXISTS (SELECT 1 FROM webauthn_credentials WHERE user_id=$1)",
    )
    .bind(user)
    .fetch_one(db)
    .await
}

/// Checks a TOTP code of the confirmed (or, with `pending`, the not yet
/// confirmed) seed and consumes its time step atomically: the same code –
/// or an older one – is never accepted twice.
pub async fn verify_totp(
    db: &PgPool,
    key: &[u8; 32],
    user: Uuid,
    code: &str,
    pending: bool,
) -> Result<bool> {
    let Some(code) = queued_api::security::normalize_totp(code) else {
        return Ok(false);
    };
    let row: Option<(Vec<u8>, i64)> = sqlx::query_as(if pending {
        "SELECT secret_enc, last_step FROM mfa_totp WHERE user_id=$1 AND confirmed_at IS NULL"
    } else {
        "SELECT secret_enc, last_step FROM mfa_totp WHERE user_id=$1 AND confirmed_at IS NOT NULL"
    })
    .bind(user)
    .fetch_optional(db)
    .await?;
    let Some((enc, last)) = row else {
        return Ok(false);
    };
    let secret = crate::crypto::decrypt(key, &enc)?;
    let Some(step) = totp_match(&secret, &code, Utc::now().timestamp()) else {
        return Ok(false);
    };
    if step <= last {
        return Ok(false);
    }
    let consumed =
        sqlx::query("UPDATE mfa_totp SET last_step=$2 WHERE user_id=$1 AND last_step < $2")
            .bind(user)
            .bind(step)
            .execute(db)
            .await?
            .rows_affected();
    Ok(consumed == 1)
}

/// Consumes a recovery code (single use, atomic).
pub async fn use_recovery(db: &PgPool, key: &[u8; 32], user: Uuid, code: &str) -> Result<bool> {
    let Some(code) = queued_api::security::normalize_recovery_code(code) else {
        return Ok(false);
    };
    let used = sqlx::query(
        "UPDATE recovery_codes SET used_at=now() WHERE user_id=$1 AND code_hash=$2 AND used_at IS NULL",
    )
    .bind(user)
    .bind(recovery_hash(key, &code))
    .execute(db)
    .await?
    .rows_affected();
    Ok(used == 1)
}

/// Replaces the recovery codes of `user`; returns the new ones (shown once).
pub async fn replace_recovery<'e>(
    tx: &mut sqlx::Transaction<'e, sqlx::Postgres>,
    key: &[u8; 32],
    user: Uuid,
) -> sqlx::Result<Vec<String>> {
    sqlx::query("DELETE FROM recovery_codes WHERE user_id=$1")
        .bind(user)
        .execute(&mut **tx)
        .await?;
    let codes = new_recovery_codes();
    for c in &codes {
        sqlx::query("INSERT INTO recovery_codes (id, user_id, code_hash) VALUES ($1,$2,$3)")
            .bind(Uuid::new_v4())
            .bind(user)
            .bind(recovery_hash(key, c))
            .execute(&mut **tx)
            .await?;
    }
    Ok(codes)
}

// ---------------------------------------------------------------------------
// Flows
// ---------------------------------------------------------------------------

pub struct Flow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub method: String,
    pub state: Option<Value>,
    pub attempts: i32,
}

/// Starts a flow; returns its secret token (only its hash is stored).
pub async fn start_flow(
    db: &PgPool,
    user: Uuid,
    kind: &str,
    method: &str,
    state: Option<Value>,
) -> sqlx::Result<String> {
    let token = crate::crypto::random_token();
    if kind != "mfa_login" {
        // One pending ceremony of a kind per person.
        sqlx::query("DELETE FROM auth_flows WHERE user_id=$1 AND kind=$2")
            .bind(user)
            .bind(kind)
            .execute(db)
            .await?;
    }
    sqlx::query(
        "INSERT INTO auth_flows (id, token_hash, user_id, kind, method, state, expires_at)
         VALUES ($1,$2,$3,$4,$5,$6, now() + make_interval(mins => $7))",
    )
    .bind(Uuid::new_v4())
    .bind(sha256_hex(&token))
    .bind(user)
    .bind(kind)
    .bind(method)
    .bind(state)
    .bind(FLOW_MINUTES as i32)
    .execute(db)
    .await?;
    Ok(token)
}

pub async fn flow_by_token(db: &PgPool, token: &str, kind: &str) -> sqlx::Result<Option<Flow>> {
    let row: Option<(Uuid, Uuid, String, Option<Value>, i32)> = sqlx::query_as(
        "SELECT id, user_id, method, state, attempts FROM auth_flows
         WHERE token_hash=$1 AND kind=$2 AND expires_at > now()",
    )
    .bind(sha256_hex(token))
    .bind(kind)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(id, user_id, method, state, attempts)| Flow {
        id,
        user_id,
        method,
        state,
        attempts,
    }))
}

pub async fn flow_of_user(db: &PgPool, user: Uuid, kind: &str) -> sqlx::Result<Option<Flow>> {
    let row: Option<(Uuid, Uuid, String, Option<Value>, i32)> = sqlx::query_as(
        "SELECT id, user_id, method, state, attempts FROM auth_flows
         WHERE user_id=$1 AND kind=$2 AND expires_at > now() ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user)
    .bind(kind)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(id, user_id, method, state, attempts)| Flow {
        id,
        user_id,
        method,
        state,
        attempts,
    }))
}

pub async fn set_flow_state(db: &PgPool, id: Uuid, state: Value) -> sqlx::Result<()> {
    sqlx::query("UPDATE auth_flows SET state=$2 WHERE id=$1")
        .bind(id)
        .bind(state)
        .execute(db)
        .await?;
    Ok(())
}

/// Counts a wrong answer; the flow ends after [`FLOW_MAX_ATTEMPTS`].
pub async fn flow_failed(db: &PgPool, id: Uuid) -> sqlx::Result<bool> {
    let n: Option<i32> = sqlx::query_scalar(
        "UPDATE auth_flows SET attempts=attempts+1 WHERE id=$1 RETURNING attempts",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    let ended = n.is_none_or(|n| n >= FLOW_MAX_ATTEMPTS);
    if ended {
        end_flow(db, id).await?;
    }
    Ok(ended)
}

/// Single use: deletes the flow; `false` if it was already gone (a second
/// request with the same flow lost the race).
pub async fn end_flow(db: &PgPool, id: Uuid) -> sqlx::Result<bool> {
    Ok(sqlx::query("DELETE FROM auth_flows WHERE id=$1")
        .bind(id)
        .execute(db)
        .await?
        .rows_affected()
        == 1)
}

pub async fn sweep(db: &PgPool) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM auth_flows WHERE expires_at < now()")
        .execute(db)
        .await?;
    sqlx::query(
        "DELETE FROM auth_throttle WHERE window_start < now() - interval '1 day'
           AND (locked_until IS NULL OR locked_until < now())",
    )
    .execute(db)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Throttle
// ---------------------------------------------------------------------------

/// `Some(until)` while the key is locked.
pub async fn locked(db: &PgPool, key: &str) -> sqlx::Result<Option<DateTime<Utc>>> {
    sqlx::query_scalar(
        "SELECT locked_until FROM auth_throttle WHERE key=$1 AND locked_until > now()",
    )
    .bind(key)
    .fetch_optional(db)
    .await
    .map(Option::flatten)
}

/// Counts a failure; returns `Some(until)` when this failure locked the key.
pub async fn failed(db: &PgPool, key: &str, max: i32) -> sqlx::Result<Option<DateTime<Utc>>> {
    sqlx::query_scalar(
        "INSERT INTO auth_throttle (key, failures, window_start) VALUES ($1, 1, now())
         ON CONFLICT (key) DO UPDATE SET
            failures = CASE WHEN auth_throttle.window_start < now() - make_interval(mins => $3)
                            THEN 1 ELSE auth_throttle.failures + 1 END,
            window_start = CASE WHEN auth_throttle.window_start < now() - make_interval(mins => $3)
                                THEN now() ELSE auth_throttle.window_start END
         RETURNING CASE WHEN failures >= $2 THEN now() + make_interval(mins => $4) END",
    )
    .bind(key)
    .bind(max)
    .bind(THROTTLE_WINDOW_MIN as i32)
    .bind(LOCK_MIN as i32)
    .fetch_one(db)
    .await
}

/// Stores the lock (after [`failed`] returned one).
pub async fn lock(db: &PgPool, key: &str, until: DateTime<Utc>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE auth_throttle SET locked_until=$2, failures=0, window_start=now() WHERE key=$1",
    )
    .bind(key)
    .bind(until)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn clear(db: &PgPool, key: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM auth_throttle WHERE key=$1")
        .bind(key)
        .execute(db)
        .await?;
    Ok(())
}

pub fn login_key(email: &str) -> String {
    format!("login:{}", email.trim().to_lowercase())
}

pub fn mfa_key(user: Uuid) -> String {
    format!("mfa:{user}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 appendix B (SHA-1 seed "12345678901234567890", 8 digits).
    #[test]
    fn rfc6238_test_vectors() {
        let seed = b"12345678901234567890";
        for (t, want) in [
            (59i64, 94287082u32),
            (1111111109, 7081804),
            (1111111111, 14050471),
            (1234567890, 89005924),
            (2000000000, 69279037),
            (20000000000, 65353130),
        ] {
            assert_eq!(hotp(seed, step_at(t) as u64, 8), want, "t={t}");
        }
        // RFC 4226 appendix D (HOTP, 6 digits).
        let hotp_vectors = [
            755224u32, 287082, 359152, 969429, 338314, 254676, 287922, 162583, 399871, 520489,
        ];
        for (c, want) in hotp_vectors.iter().enumerate() {
            assert_eq!(hotp(seed, c as u64, 6), *want, "counter {c}");
        }
    }

    #[test]
    fn totp_window_and_newest_step() {
        let seed = b"12345678901234567890";
        let t = 1_700_000_000i64;
        let now = step_at(t);
        let code = |s: i64| format!("{:06}", hotp(seed, s as u64, 6));
        assert_eq!(totp_match(seed, &code(now), t), Some(now));
        assert_eq!(totp_match(seed, &code(now - 1), t), Some(now - 1));
        assert_eq!(totp_match(seed, &code(now + 1), t), Some(now + 1));
        assert_eq!(
            totp_match(seed, &code(now - 2), t),
            None,
            "outside the window"
        );
        assert_eq!(totp_match(seed, &code(now + 2), t), None);
        assert_eq!(totp_match(seed, "abcdef", t), None);
    }

    #[test]
    fn base32_roundtrip_and_rfc4648_vectors() {
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_encode(b"f"), "MY");
        assert_eq!(base32_decode("MZXW6YTBOI").unwrap(), b"foobar");
        assert_eq!(base32_decode("mzxw 6ytb oi======").unwrap(), b"foobar");
        assert!(base32_decode("MZ1").is_none());
        let s = new_totp_secret();
        assert_eq!(base32_decode(&base32_encode(&s)).unwrap(), s);
        assert_eq!(base32_encode(&s).len(), 32);
    }

    #[test]
    fn uri_and_recovery_codes() {
        let u = totp_uri("Queue (example.org)", "a+b@example.org", "ABC");
        assert!(u.starts_with("otpauth://totp/Queue%20%28example.org%29:a%2Bb%40example.org?secret=ABC&issuer=Queue%20%28example.org%29"));
        let codes = new_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODES);
        for c in &codes {
            assert_eq!(
                queued_api::security::normalize_recovery_code(c).as_deref(),
                Some(c.as_str())
            );
        }
        let k = [1u8; 32];
        assert_eq!(recovery_hash(&k, "ABCDE"), recovery_hash(&k, "ABCDE"));
        assert_ne!(
            recovery_hash(&k, "ABCDE"),
            recovery_hash(&[2u8; 32], "ABCDE")
        );
        assert_ne!(recovery_hash(&k, "ABCDE"), sha256_hex("ABCDE"));
    }

    #[test]
    fn relying_party_from_public_url() {
        assert!(webauthn("https://queue.example.org").is_ok());
        assert!(webauthn("http://localhost:8080").is_ok());
        assert!(webauthn("not a url").is_err());
    }
}
