//! Web Push (VAPID) – works for installed PWAs incl. iOS/iPadOS.
use std::path::Path;

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use web_push::{ContentEncoding, SubscriptionInfo, VapidSignatureBuilder, WebPushMessageBuilder};

use crate::{config::var, state::AppState};

pub struct Vapid {
    pub private_b64: String,
    pub public_b64: String,
}

impl Vapid {
    pub fn load_or_create(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join("vapid_private.key");
        let private_b64 = if let Some(k) = var("VAPID_PRIVATE_KEY") {
            k
        } else if path.exists() {
            std::fs::read_to_string(&path)?.trim().to_string()
        } else {
            // A random 32-byte scalar is a valid P-256 key with overwhelming probability.
            let key = loop {
                let mut b = [0u8; 32];
                rand::thread_rng().fill_bytes(&mut b);
                let k = URL_SAFE_NO_PAD.encode(b);
                if VapidSignatureBuilder::from_base64_no_sub(&k, web_push::URL_SAFE_NO_PAD).is_ok()
                {
                    break k;
                }
            };
            std::fs::write(&path, &key)?;
            key
        };
        let partial =
            VapidSignatureBuilder::from_base64_no_sub(&private_b64, web_push::URL_SAFE_NO_PAD)
                .map_err(|e| anyhow!("invalid VAPID key: {e:?}"))?;
        Ok(Self {
            public_b64: URL_SAFE_NO_PAD.encode(partial.get_public_key()),
            private_b64,
        })
    }
}

/// Returns Ok(false) if the subscription is gone and should be deleted.
pub async fn send(
    st: &AppState,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
    payload: &[u8],
) -> Result<bool> {
    let info = SubscriptionInfo::new(endpoint, p256dh, auth);
    let mut sig =
        VapidSignatureBuilder::from_base64(&st.vapid.private_b64, web_push::URL_SAFE_NO_PAD, &info)
            .map_err(|e| anyhow!("vapid: {e:?}"))?;
    sig.add_claim("sub", st.cfg.vapid_subject.as_str());
    let sig = sig.build().map_err(|e| anyhow!("vapid: {e:?}"))?;

    let mut builder = WebPushMessageBuilder::new(&info);
    builder.set_payload(ContentEncoding::Aes128Gcm, payload);
    builder.set_vapid_signature(sig);
    builder.set_ttl(24 * 3600);
    let msg = builder.build().map_err(|e| anyhow!("push build: {e:?}"))?;
    let req = web_push::request_builder::build_request::<Vec<u8>>(msg);

    let mut rb = st.http.post(req.uri().to_string());
    for (k, v) in req.headers() {
        rb = rb.header(k.as_str(), v.as_bytes());
    }
    let resp = rb.body(req.into_body()).send().await?;
    match resp.status().as_u16() {
        200..=299 => Ok(true),
        404 | 410 => Ok(false),
        s => Err(anyhow!("push service returned {s}")),
    }
}
