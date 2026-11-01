//! OIDC / OAuth2 login (Google, Apple, GitHub, Keycloak, Authentik, Entra, …).
//!
//! Configured via env:
//!   OIDC_PROVIDERS=google,apple,github,keycloak
//!   OIDC_<ID>_CLIENT_ID, OIDC_<ID>_CLIENT_SECRET, OIDC_<ID>_ISSUER (discovery),
//!   or explicit OIDC_<ID>_AUTH_URL / _TOKEN_URL / _USERINFO_URL, optional _SCOPES, _NAME.
//!   Apple: OIDC_APPLE_TEAM_ID, OIDC_APPLE_KEY_ID, OIDC_APPLE_PRIVATE_KEY (PEM of the .p8 key).
//!
//! The ID token is received directly from the token endpoint over TLS, so per
//! OIDC Core 3.1.3.7 TLS server validation substitutes the signature check; we
//! still validate iss/aud/exp/nonce.
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::Value;

use crate::config::var;

#[derive(Debug, Clone)]
pub struct AppleKey {
    pub team_id: String,
    pub key_id: String,
    pub private_key_pem: String,
}

#[derive(Debug, Clone)]
pub struct OidcProvider {
    pub id: String,
    pub name: String,
    pub issuer: Option<String>,
    pub auth_url: String,
    pub token_url: String,
    pub userinfo_url: Option<String>,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: String,
    pub apple: Option<AppleKey>,
    pub pkce: bool,
    pub form_post: bool,
}

#[derive(Debug, Clone)]
pub struct ExternalIdentity {
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
}

pub async fn load_providers(http: &reqwest::Client) -> Vec<OidcProvider> {
    let Some(list) = var("OIDC_PROVIDERS") else { return vec![] };
    let mut out = vec![];
    for id in list.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()) {
        match load_one(http, &id).await {
            Ok(p) => {
                tracing::info!("OIDC provider '{}' enabled", p.id);
                out.push(p)
            }
            Err(e) => tracing::error!("OIDC provider '{id}' disabled: {e:#}"),
        }
    }
    out
}

async fn load_one(http: &reqwest::Client, id: &str) -> Result<OidcProvider> {
    let env = |k: &str| var(&format!("OIDC_{}_{k}", id.to_uppercase()));
    let client_id = env("CLIENT_ID").context("CLIENT_ID missing")?;
    let is_apple = id == "apple";
    let is_github = id == "github";

    let issuer = env("ISSUER").or_else(|| match id {
        "google" => Some("https://accounts.google.com".into()),
        "apple" => Some("https://appleid.apple.com".into()),
        _ => None,
    });

    let (mut auth_url, mut token_url, mut userinfo_url) = (env("AUTH_URL"), env("TOKEN_URL"), env("USERINFO_URL"));
    if is_github {
        auth_url = auth_url.or(Some("https://github.com/login/oauth/authorize".into()));
        token_url = token_url.or(Some("https://github.com/login/oauth/access_token".into()));
        userinfo_url = userinfo_url.or(Some("https://api.github.com/user".into()));
    }
    if auth_url.is_none() || token_url.is_none() {
        let iss = issuer.as_deref().context("ISSUER or AUTH_URL/TOKEN_URL required")?;
        let disc: Value = http
            .get(format!("{}/.well-known/openid-configuration", iss.trim_end_matches('/')))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let s = |k: &str| disc.get(k).and_then(|v| v.as_str()).map(String::from);
        auth_url = auth_url.or(s("authorization_endpoint"));
        token_url = token_url.or(s("token_endpoint"));
        userinfo_url = userinfo_url.or(s("userinfo_endpoint"));
    }

    let apple = if is_apple {
        Some(AppleKey {
            team_id: env("TEAM_ID").context("TEAM_ID missing")?,
            key_id: env("KEY_ID").context("KEY_ID missing")?,
            private_key_pem: env("PRIVATE_KEY").context("PRIVATE_KEY missing")?.replace("\\n", "\n"),
        })
    } else {
        None
    };
    if !is_apple && env("CLIENT_SECRET").is_none() {
        tracing::warn!("OIDC '{id}': no CLIENT_SECRET, using public client + PKCE");
    }

    let default_name = match id {
        "google" => "Google".to_string(),
        "apple" => "Apple".to_string(),
        "github" => "GitHub".to_string(),
        other => {
            let mut c = other.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
    };

    Ok(OidcProvider {
        id: id.to_string(),
        name: env("NAME").unwrap_or(default_name),
        scopes: env("SCOPES").unwrap_or_else(|| {
            match id {
                "apple" => "openid name email",
                "github" => "read:user user:email",
                _ => "openid email profile",
            }
            .into()
        }),
        issuer: if is_github { None } else { issuer },
        auth_url: auth_url.context("no authorization endpoint")?,
        token_url: token_url.context("no token endpoint")?,
        userinfo_url,
        client_id,
        client_secret: env("CLIENT_SECRET"),
        apple,
        pkce: !is_apple,
        form_post: is_apple,
    })
}

impl OidcProvider {
    pub fn redirect_uri(&self, public_url: &str) -> String {
        format!("{public_url}/api/auth/oidc/{}/callback", self.id)
    }

    pub fn authorize_url(&self, public_url: &str, state: &str, nonce: &str, verifier: &str) -> Result<String> {
        let mut url = url::Url::parse(&self.auth_url)?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("response_type", "code")
                .append_pair("client_id", &self.client_id)
                .append_pair("redirect_uri", &self.redirect_uri(public_url))
                .append_pair("scope", &self.scopes)
                .append_pair("state", state)
                .append_pair("nonce", nonce);
            if self.pkce {
                q.append_pair("code_challenge", &crate::crypto::sha256_b64url(verifier))
                    .append_pair("code_challenge_method", "S256");
            }
            if self.form_post {
                q.append_pair("response_mode", "form_post");
            }
        }
        Ok(url.into())
    }

    fn client_secret(&self) -> Result<Option<String>> {
        if let Some(a) = &self.apple {
            // Apple: client_secret is a short-lived ES256 JWT signed with the .p8 key.
            let now = chrono::Utc::now().timestamp();
            let claims = serde_json::json!({
                "iss": a.team_id, "iat": now, "exp": now + 300,
                "aud": "https://appleid.apple.com", "sub": self.client_id,
            });
            let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
            header.kid = Some(a.key_id.clone());
            let key = jsonwebtoken::EncodingKey::from_ec_pem(a.private_key_pem.as_bytes())?;
            return Ok(Some(jsonwebtoken::encode(&header, &claims, &key)?));
        }
        Ok(self.client_secret.clone())
    }

    pub async fn exchange(
        &self,
        http: &reqwest::Client,
        public_url: &str,
        code: &str,
        verifier: &str,
        nonce: &str,
        apple_user: Option<&str>,
    ) -> Result<ExternalIdentity> {
        let redirect = self.redirect_uri(public_url);
        let secret = self.client_secret()?;
        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &redirect),
            ("client_id", &self.client_id),
        ];
        if let Some(s) = &secret {
            form.push(("client_secret", s));
        }
        if self.pkce {
            form.push(("code_verifier", verifier));
        }
        let resp = http.post(&self.token_url).header("Accept", "application/json").form(&form).send().await?;
        let status = resp.status();
        let tok: Value = resp.json().await.context("token response not JSON")?;
        if !status.is_success() || tok.get("error").is_some() {
            bail!("token endpoint error {status}: {tok}");
        }

        let mut claims = Value::Object(Default::default());
        if let Some(id_token) = tok.get("id_token").and_then(|v| v.as_str()) {
            claims = decode_jwt_payload(id_token)?;
            self.validate_id_token(&claims, nonce)?;
        }
        if claims.get("sub").is_none() {
            let access = tok.get("access_token").and_then(|v| v.as_str()).context("no access_token")?;
            let ui = self.userinfo_url.as_deref().context("no id_token and no userinfo endpoint")?;
            claims = http.get(ui).bearer_auth(access).send().await?.error_for_status()?.json().await?;
            if self.id == "github" && claims.get("email").and_then(|v| v.as_str()).is_none() {
                if let Ok(r) = http.get("https://api.github.com/user/emails").bearer_auth(access).send().await {
                    let emails: Vec<Value> = r.json().await.unwrap_or_default();
                    if let Some(p) = emails.iter().find(|e| e["primary"].as_bool() == Some(true)) {
                        claims["email"] = p["email"].clone();
                        claims["email_verified"] = p["verified"].clone();
                    }
                }
            }
        }

        let subject = match claims.get("sub").or_else(|| claims.get("id")) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => bail!("identity has no subject"),
        };
        let email_verified = match claims.get("email_verified") {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => s == "true",
            _ => false,
        };
        let mut name = claims
            .get("name")
            .or_else(|| claims.get("login"))
            .and_then(|v| v.as_str())
            .map(String::from);
        // Apple sends the name only once, as a form field on first login.
        if let Some(u) = apple_user.and_then(|u| serde_json::from_str::<Value>(u).ok()) {
            let first = u["name"]["firstName"].as_str().unwrap_or_default();
            let last = u["name"]["lastName"].as_str().unwrap_or_default();
            let full = format!("{first} {last}").trim().to_string();
            if !full.is_empty() {
                name = Some(full);
            }
        }
        Ok(ExternalIdentity {
            subject,
            email: claims.get("email").and_then(|v| v.as_str()).map(|s| s.to_lowercase()),
            email_verified,
            name,
        })
    }

    fn validate_id_token(&self, c: &Value, nonce: &str) -> Result<()> {
        if let Some(iss) = &self.issuer {
            let got = c["iss"].as_str().unwrap_or_default();
            if got.trim_end_matches('/') != iss.trim_end_matches('/') {
                bail!("id_token iss mismatch: {got}");
            }
        }
        let aud_ok = match &c["aud"] {
            Value::String(s) => s == &self.client_id,
            Value::Array(a) => a.iter().any(|v| v.as_str() == Some(&self.client_id)),
            _ => false,
        };
        if !aud_ok {
            bail!("id_token aud mismatch");
        }
        if c["exp"].as_i64().unwrap_or(0) < chrono::Utc::now().timestamp() {
            bail!("id_token expired");
        }
        if c["nonce"].as_str() != Some(nonce) {
            bail!("id_token nonce mismatch");
        }
        Ok(())
    }
}

fn decode_jwt_payload(jwt: &str) -> Result<Value> {
    let payload = jwt.split('.').nth(1).ok_or_else(|| anyhow!("malformed JWT"))?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('='))?;
    Ok(serde_json::from_slice(&bytes)?)
}
