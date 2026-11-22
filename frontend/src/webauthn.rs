//! WP11: WebAuthn ceremonies in the browser. The server sends the options
//! of webauthn-rs as JSON (binary members base64url); here they become
//! `ArrayBuffer`s for `navigator.credentials.create/get`, and the answer is
//! turned back into the JSON webauthn-rs expects.
use js_sys::{Array, ArrayBuffer, Object, Reflect, Uint8Array};
use serde_json::Value;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::logic::{
    b64,
    security::{b64url, strip_nulls, BINARY_LIST_FIELDS},
};

fn err(msg: &str) -> String {
    msg.to_string()
}

fn get(o: &JsValue, k: &str) -> JsValue {
    Reflect::get(o, &JsValue::from_str(k)).unwrap_or(JsValue::UNDEFINED)
}

fn bytes_from_b64(v: &JsValue) -> Result<Uint8Array, String> {
    let s = v
        .as_string()
        .ok_or_else(|| err("Ungültige Antwort vom Server."))?;
    let data = b64::decode(&s).ok_or_else(|| err("Ungültige Antwort vom Server."))?;
    Ok(Uint8Array::from(data.as_slice()))
}

fn buf_to_b64(v: &JsValue) -> Option<String> {
    if v.is_undefined() || v.is_null() {
        return None;
    }
    let arr = if let Some(b) = v.dyn_ref::<ArrayBuffer>() {
        Uint8Array::new(b)
    } else {
        Uint8Array::new(v)
    };
    Some(b64url(&arr.to_vec()))
}

/// `{"publicKey": {...}}` of the server → JS options with ArrayBuffers.
fn options(server: &Value) -> Result<JsValue, String> {
    let mut v = server.clone();
    strip_nulls(&mut v);
    let pk = v
        .get("publicKey")
        .cloned()
        .ok_or_else(|| err("Ungültige Antwort vom Server."))?;
    let js =
        js_sys::JSON::parse(&pk.to_string()).map_err(|_| err("Ungültige Antwort vom Server."))?;
    let challenge = bytes_from_b64(&get(&js, "challenge"))?;
    let _ = Reflect::set(&js, &"challenge".into(), &challenge);
    let user = get(&js, "user");
    if user.is_object() {
        let id = bytes_from_b64(&get(&user, "id"))?;
        let _ = Reflect::set(&user, &"id".into(), &id);
    }
    for list in BINARY_LIST_FIELDS {
        let l = get(&js, list);
        if let Some(arr) = l.dyn_ref::<Array>() {
            for item in arr.iter() {
                let id = bytes_from_b64(&get(&item, "id"))?;
                let _ = Reflect::set(&item, &"id".into(), &id);
            }
        }
    }
    let outer = Object::new();
    let _ = Reflect::set(&outer, &"publicKey".into(), &js);
    Ok(outer.into())
}

fn container() -> Result<web_sys::CredentialsContainer, String> {
    let w = web_sys::window().ok_or_else(|| err("Kein Browserfenster."))?;
    let nav = w.navigator();
    if get(&w.into(), "PublicKeyCredential").is_undefined() {
        return Err(err("Dieser Browser unterstützt keine Passkeys."));
    }
    Ok(nav.credentials())
}

fn dom_error(e: JsValue) -> String {
    let name = get(&e, "name").as_string().unwrap_or_default();
    match name.as_str() {
        "NotAllowedError" => "Abgebrochen oder Zeit abgelaufen. Bitte versuch es erneut.".into(),
        "InvalidStateError" => "Dieser Passkey ist schon eingerichtet.".into(),
        "SecurityError" => {
            "Passkeys brauchen eine sichere Verbindung (https) zur richtigen Adresse.".into()
        }
        _ => format!("Passkey fehlgeschlagen ({name})."),
    }
}

fn base(cred: &JsValue) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert(
        "id".into(),
        Value::String(get(cred, "id").as_string().unwrap_or_default()),
    );
    m.insert(
        "rawId".into(),
        Value::String(buf_to_b64(&get(cred, "rawId")).unwrap_or_default()),
    );
    m.insert("type".into(), Value::String("public-key".into()));
    m.insert("extensions".into(), Value::Object(Default::default()));
    m
}

/// Registration: `navigator.credentials.create` → JSON for the server.
pub async fn create(server: &Value) -> Result<Value, String> {
    let opts = options(server)?;
    let c = container()?;
    let opts: web_sys::CredentialCreationOptions = opts.unchecked_into();
    let p = c.create_with_options(&opts).map_err(dom_error)?;
    let cred = JsFuture::from(p).await.map_err(dom_error)?;
    let resp = get(&cred, "response");
    let mut m = base(&cred);
    let mut r = serde_json::Map::new();
    r.insert(
        "attestationObject".into(),
        Value::String(buf_to_b64(&get(&resp, "attestationObject")).unwrap_or_default()),
    );
    r.insert(
        "clientDataJSON".into(),
        Value::String(buf_to_b64(&get(&resp, "clientDataJSON")).unwrap_or_default()),
    );
    m.insert("response".into(), Value::Object(r));
    Ok(Value::Object(m))
}

/// Authentication: `navigator.credentials.get` → JSON for the server.
pub async fn get_assertion(server: &Value) -> Result<Value, String> {
    let opts = options(server)?;
    let c = container()?;
    let opts: web_sys::CredentialRequestOptions = opts.unchecked_into();
    let p = c.get_with_options(&opts).map_err(dom_error)?;
    let cred = JsFuture::from(p).await.map_err(dom_error)?;
    let resp = get(&cred, "response");
    let mut m = base(&cred);
    let mut r = serde_json::Map::new();
    for (k, js) in [
        ("authenticatorData", "authenticatorData"),
        ("clientDataJSON", "clientDataJSON"),
        ("signature", "signature"),
    ] {
        r.insert(
            k.into(),
            Value::String(buf_to_b64(&get(&resp, js)).unwrap_or_default()),
        );
    }
    if let Some(h) = buf_to_b64(&get(&resp, "userHandle")) {
        r.insert("userHandle".into(), Value::String(h));
    }
    m.insert("response".into(), Value::Object(r));
    Ok(Value::Object(m))
}

/// The browser can do passkeys at all.
pub fn supported() -> bool {
    web_sys::window().is_some_and(|w| !get(&w.into(), "PublicKeyCredential").is_undefined())
}
