//! Web Push: RFC 8291 aes128gcm payload encryption + RFC 8292 VAPID auth,
//! implemented directly on pure-Rust crypto (p256 ECDH, HKDF-SHA256,
//! AES-128-GCM) and sent through the supervisor's existing hyper client.
//! No openssl, no push-service crate. Validated end-to-end by a WebCrypto
//! decryption test on the Bun side (see spikes/e2e notes).

use anyhow::Context;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use data_encoding::BASE64URL_NOPAD;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

use crate::AppState;

const VAPID_PRIVATE_KEY: &str = "vapid_private_key";
const VAPID_PUBLIC_KEY: &str = "vapid_public_key";
const VAPID_SUBJECT: &str = "mailto:liquid@localhost";
const JWT_TTL_SECS: i64 = 12 * 60 * 60;
const PUSH_TTL_SECS: u32 = 24 * 60 * 60;

/// Load the VAPID keypair, generating and persisting one on first use.
fn vapid_keys(state: &AppState) -> anyhow::Result<(SigningKey, String)> {
    if let (Some(private_b64), Some(public_b64)) = (
        state.db.get_setting(VAPID_PRIVATE_KEY)?,
        state.db.get_setting(VAPID_PUBLIC_KEY)?,
    ) {
        let bytes = BASE64URL_NOPAD
            .decode(private_b64.as_bytes())
            .context("stored VAPID key is corrupt")?;
        let key = SigningKey::from_slice(&bytes).context("stored VAPID key is invalid")?;
        return Ok((key, public_b64));
    }
    let key = SigningKey::random(&mut rand_core_adapter());
    let private_b64 = BASE64URL_NOPAD.encode(&key.to_bytes());
    let public_b64 = BASE64URL_NOPAD.encode(
        key.verifying_key()
            .as_affine()
            .to_encoded_point(false)
            .as_bytes(),
    );
    state.db.set_setting(VAPID_PRIVATE_KEY, &private_b64)?;
    state.db.set_setting(VAPID_PUBLIC_KEY, &public_b64)?;
    info!("generated VAPID keypair");
    Ok((key, public_b64))
}

/// p256 0.13 wants a rand_core 0.6 CryptoRngCore; adapt rand 0.9's OS rng.
fn rand_core_adapter() -> impl p256::elliptic_curve::rand_core::CryptoRngCore {
    struct Adapter;
    impl p256::elliptic_curve::rand_core::RngCore for Adapter {
        fn next_u32(&mut self) -> u32 {
            let mut b = [0u8; 4];
            self.fill_bytes(&mut b);
            u32::from_le_bytes(b)
        }
        fn next_u64(&mut self) -> u64 {
            let mut b = [0u8; 8];
            self.fill_bytes(&mut b);
            u64::from_le_bytes(b)
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            use rand::RngCore;
            rand::rng().fill_bytes(dest);
        }
        fn try_fill_bytes(
            &mut self,
            dest: &mut [u8],
        ) -> Result<(), p256::elliptic_curve::rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }
    impl p256::elliptic_curve::rand_core::CryptoRng for Adapter {}
    Adapter
}

/// VAPID JWT: ES256 over {aud, exp, sub} where aud is the push service origin.
fn vapid_jwt(key: &SigningKey, audience: &str) -> String {
    let header = BASE64URL_NOPAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let exp = chrono::Utc::now().timestamp() + JWT_TTL_SECS;
    let payload = BASE64URL_NOPAD.encode(
        json!({ "aud": audience, "exp": exp, "sub": VAPID_SUBJECT })
            .to_string()
            .as_bytes(),
    );
    let signing_input = format!("{header}.{payload}");
    let signature: Signature = key.sign(signing_input.as_bytes());
    let sig_b64 = BASE64URL_NOPAD.encode(&signature.to_bytes());
    format!("{signing_input}.{sig_b64}")
}

/// Send a notification to every stored subscription. Dead subscriptions
/// (404/410 from the push service) are pruned.
pub async fn notify_all(state: &AppState, title: &str, body: &str) {
    let subscriptions = match state.db.list_push_subscriptions() {
        Ok(subs) => subs,
        Err(err) => {
            warn!("could not list push subscriptions: {err:#}");
            return;
        }
    };
    if subscriptions.is_empty() {
        return;
    }
    let (key, _) = match vapid_keys(state) {
        Ok(keys) => keys,
        Err(err) => {
            warn!("VAPID keys unavailable: {err:#}");
            return;
        }
    };
    let payload = json!({ "title": title, "body": body }).to_string();

    for (endpoint, p256dh, auth) in subscriptions {
        match send_one(state, &key, &endpoint, &p256dh, &auth, payload.as_bytes()).await {
            Ok(status) if status == StatusCode::NOT_FOUND || status == StatusCode::GONE => {
                info!("pruning dead push subscription");
                let _ = state.db.remove_push_subscription(&endpoint);
            }
            Ok(status) if !status.is_success() => {
                warn!("push service returned {status} for {endpoint}");
            }
            Ok(_) => {}
            Err(err) => warn!("push send failed: {err:#}"),
        }
    }
}

async fn send_one(
    state: &AppState,
    key: &SigningKey,
    endpoint: &str,
    p256dh_b64: &str,
    auth_b64: &str,
    payload: &[u8],
) -> anyhow::Result<StatusCode> {
    let p256dh = BASE64URL_NOPAD
        .decode(p256dh_b64.trim_end_matches('=').as_bytes())
        .context("bad p256dh")?;
    let auth = BASE64URL_NOPAD
        .decode(auth_b64.trim_end_matches('=').as_bytes())
        .context("bad auth secret")?;
    let ciphertext =
        rfc8291::encrypt(&p256dh, &auth, payload).context("aes128gcm encryption failed")?;

    let uri: axum::http::Uri = endpoint.parse().context("bad endpoint uri")?;
    let audience = format!(
        "{}://{}",
        uri.scheme_str().unwrap_or("https"),
        uri.authority().map(|a| a.as_str()).unwrap_or_default()
    );
    let jwt = vapid_jwt(key, &audience);
    let public_b64 = state
        .db
        .get_setting(VAPID_PUBLIC_KEY)?
        .context("missing VAPID public key")?;

    let request = axum::http::Request::builder()
        .method("POST")
        .uri(endpoint)
        .header("TTL", PUSH_TTL_SECS)
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header("Authorization", format!("vapid t={jwt}, k={public_b64}"))
        .body(axum::body::Body::from(ciphertext))
        .context("building push request")?;

    let response = state
        .http_client
        .request(request)
        .await
        .context("push request failed")?;
    Ok(response.status())
}

/// RFC 8291 "aes128gcm" content encoding for Web Push, single record.
mod rfc8291 {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes128Gcm, KeyInit, Nonce};
    use anyhow::Context;
    use hkdf::Hkdf;
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    use sha2::Sha256;

    const RECORD_SIZE: u32 = 4096;
    const KEY_INFO_PREFIX: &[u8] = b"WebPush: info\0";
    const CEK_INFO: &[u8] = b"Content-Encoding: aes128gcm\0";
    const NONCE_INFO: &[u8] = b"Content-Encoding: nonce\0";
    /// 0x02 marks the final (only) record.
    const LAST_RECORD_DELIMITER: u8 = 0x02;

    /// Encrypt `plaintext` for a browser push subscription.
    /// `receiver_pub` is the subscription's p256dh (65-byte uncompressed
    /// point), `auth` its 16-byte auth secret.
    pub fn encrypt(receiver_pub: &[u8], auth: &[u8], plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut salt = [0u8; 16];
        {
            use rand::RngCore;
            rand::rng().fill_bytes(&mut salt);
        }
        let ephemeral = p256::SecretKey::random(&mut super::rand_core_adapter());
        encrypt_with(&salt, &ephemeral, receiver_pub, auth, plaintext)
    }

    /// Deterministic core, separated so tests can inject salt + ephemeral key.
    pub fn encrypt_with(
        salt: &[u8; 16],
        ephemeral: &p256::SecretKey,
        receiver_pub: &[u8],
        auth: &[u8],
        plaintext: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        let receiver = p256::PublicKey::from_sec1_bytes(receiver_pub)
            .map_err(|_| anyhow::anyhow!("subscription p256dh is not a valid P-256 point"))?;
        let ephemeral_pub = ephemeral.public_key().to_encoded_point(false);

        // ecdh_secret = ECDH(as_private, ua_public)
        let shared = p256::ecdh::diffie_hellman(ephemeral.to_nonzero_scalar(), receiver.as_affine());

        // IKM = HKDF(salt=auth, ikm=ecdh, info="WebPush: info"||0||ua_pub||as_pub, 32)
        let mut key_info = Vec::with_capacity(KEY_INFO_PREFIX.len() + 130);
        key_info.extend_from_slice(KEY_INFO_PREFIX);
        key_info.extend_from_slice(receiver_pub);
        key_info.extend_from_slice(ephemeral_pub.as_bytes());
        let mut ikm = [0u8; 32];
        Hkdf::<Sha256>::new(Some(auth), shared.raw_secret_bytes())
            .expand(&key_info, &mut ikm)
            .map_err(|_| anyhow::anyhow!("hkdf ikm expand failed"))?;

        // CEK (16) and NONCE (12) from HKDF(salt=salt, ikm=IKM)
        let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
        let mut cek = [0u8; 16];
        prk.expand(CEK_INFO, &mut cek)
            .map_err(|_| anyhow::anyhow!("hkdf cek expand failed"))?;
        let mut nonce = [0u8; 12];
        prk.expand(NONCE_INFO, &mut nonce)
            .map_err(|_| anyhow::anyhow!("hkdf nonce expand failed"))?;

        // record = plaintext || 0x02, single-record message
        let mut record = Vec::with_capacity(plaintext.len() + 1);
        record.extend_from_slice(plaintext);
        record.push(LAST_RECORD_DELIMITER);
        let cipher = Aes128Gcm::new_from_slice(&cek).context("bad cek length")?;
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), record.as_slice())
            .map_err(|_| anyhow::anyhow!("aes-gcm encryption failed"))?;

        // header = salt(16) || rs(4) || idlen(1) || keyid(as_public, 65)
        let mut body =
            Vec::with_capacity(16 + 4 + 1 + ephemeral_pub.as_bytes().len() + ciphertext.len());
        body.extend_from_slice(salt);
        body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
        body.push(ephemeral_pub.as_bytes().len() as u8);
        body.extend_from_slice(ephemeral_pub.as_bytes());
        body.extend_from_slice(&ciphertext);
        Ok(body)
    }
}

// --- endpoints ---------------------------------------------------------------------

pub async fn public_key(State(state): State<AppState>) -> Response {
    match vapid_keys(&state) {
        Ok((_, public_b64)) => Json(json!({ "key": public_b64 })).into_response(),
        Err(err) => {
            warn!("vapid keygen failed: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct SubscriptionBody {
    endpoint: String,
    keys: SubscriptionKeys,
}

#[derive(Deserialize)]
pub struct SubscriptionKeys {
    p256dh: String,
    auth: String,
}

pub async fn subscribe(
    State(state): State<AppState>,
    Json(body): Json<SubscriptionBody>,
) -> Response {
    match state
        .db
        .add_push_subscription(&body.endpoint, &body.keys.p256dh, &body.keys.auth)
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            warn!("subscribe failed: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct UnsubscribeBody {
    endpoint: String,
}

pub async fn unsubscribe(
    State(state): State<AppState>,
    Json(body): Json<UnsubscribeBody>,
) -> Response {
    match state.db.remove_push_subscription(&body.endpoint) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            warn!("unsubscribe failed: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn test(State(state): State<AppState>) -> Response {
    notify_all(&state, "liquid", "Push notifications are working 💧").await;
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;

    #[test]
    fn jwt_is_well_formed_and_verifiable() {
        let key = SigningKey::random(&mut rand_core_adapter());
        let jwt = vapid_jwt(&key, "https://push.example.com");
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);

        let header: serde_json::Value =
            serde_json::from_slice(&BASE64URL_NOPAD.decode(parts[0].as_bytes()).unwrap()).unwrap();
        assert_eq!(header["alg"], "ES256");
        let payload: serde_json::Value =
            serde_json::from_slice(&BASE64URL_NOPAD.decode(parts[1].as_bytes()).unwrap()).unwrap();
        assert_eq!(payload["aud"], "https://push.example.com");
        assert_eq!(payload["sub"], VAPID_SUBJECT);
        assert!(payload["exp"].as_i64().unwrap() > chrono::Utc::now().timestamp());

        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig_bytes = BASE64URL_NOPAD.decode(parts[2].as_bytes()).unwrap();
        let signature = Signature::from_slice(&sig_bytes).unwrap();
        key.verifying_key()
            .verify(signing_input.as_bytes(), &signature)
            .expect("signature verifies");
    }

    #[test]
    fn payload_encrypts_for_a_real_looking_subscription() {
        // Simulate a browser subscription: its own p256 keypair + 16-byte auth.
        let browser_key = p256::SecretKey::random(&mut rand_core_adapter());
        let browser_pub = browser_key.public_key().to_encoded_point(false);
        let auth = [7u8; 16];
        let plaintext = b"hello push";
        let body = rfc8291::encrypt(browser_pub.as_bytes(), &auth, plaintext).unwrap();

        // header: salt(16) || rs=4096(4) || idlen=65(1) || keyid(65)
        assert_eq!(&body[16..20], &4096u32.to_be_bytes());
        assert_eq!(body[20], 65);
        assert_eq!(body[21], 0x04); // uncompressed point marker
        // ciphertext = record(plaintext+1 delimiter) + 16-byte GCM tag
        assert_eq!(body.len(), 86 + plaintext.len() + 1 + 16);
        // plaintext must not appear anywhere in the output
        assert!(!body.windows(plaintext.len()).any(|w| w == plaintext));
    }

    /// Encrypt as the server, then decrypt as the *browser* would (RFC 8291
    /// receiver side): recover the shared secret from the subscription's
    /// PRIVATE key and the ephemeral public key in the header, rerun HKDF, and
    /// AES-GCM-decrypt. Recovering the plaintext this way proves the ECDH
    /// direction, HKDF derivation, and GCM framing are interoperable — the
    /// exact computation a browser performs.
    #[test]
    fn browser_side_decryption_recovers_plaintext() {
        use aes_gcm::aead::Aead;
        use aes_gcm::{Aes128Gcm, KeyInit, Nonce};
        use hkdf::Hkdf;
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        use sha2::Sha256;

        let plaintext = b"When I grow up, I want to be a watermelon";
        // The "browser": a subscription keypair + 16-byte auth secret.
        let ua_secret = p256::SecretKey::random(&mut rand_core_adapter());
        let ua_public = ua_secret.public_key().to_encoded_point(false);
        let auth = [0x42u8; 16];

        let body =
            rfc8291::encrypt(ua_public.as_bytes(), &auth, plaintext).expect("encrypt");

        // Parse the aes128gcm header.
        let salt = &body[0..16];
        let idlen = body[20] as usize;
        let as_public_bytes = &body[21..21 + idlen];
        let ciphertext = &body[21 + idlen..];
        let as_public = p256::PublicKey::from_sec1_bytes(as_public_bytes).unwrap();

        // Receiver ECDH: ua_private × as_public (mirror of the sender's dir).
        let shared =
            p256::ecdh::diffie_hellman(ua_secret.to_nonzero_scalar(), as_public.as_affine());

        let mut key_info = Vec::new();
        key_info.extend_from_slice(b"WebPush: info\0");
        key_info.extend_from_slice(ua_public.as_bytes());
        key_info.extend_from_slice(as_public_bytes);
        let mut ikm = [0u8; 32];
        Hkdf::<Sha256>::new(Some(&auth), shared.raw_secret_bytes())
            .expand(&key_info, &mut ikm)
            .unwrap();

        let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
        let mut cek = [0u8; 16];
        prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).unwrap();
        let mut nonce = [0u8; 12];
        prk.expand(b"Content-Encoding: nonce\0", &mut nonce).unwrap();

        let record = Aes128Gcm::new_from_slice(&cek)
            .unwrap()
            .decrypt(Nonce::from_slice(&nonce), ciphertext)
            .expect("browser decrypts server ciphertext");
        // Strip the single-record 0x02 delimiter.
        assert_eq!(*record.last().unwrap(), 0x02);
        assert_eq!(&record[..record.len() - 1], plaintext);
    }

    #[test]
    fn encryption_is_deterministic_given_salt_and_key() {
        let browser_key = p256::SecretKey::random(&mut rand_core_adapter());
        let browser_pub = browser_key.public_key().to_encoded_point(false);
        let ephemeral = p256::SecretKey::random(&mut rand_core_adapter());
        let salt = [9u8; 16];
        let auth = [7u8; 16];
        let a = rfc8291::encrypt_with(&salt, &ephemeral, browser_pub.as_bytes(), &auth, b"x").unwrap();
        let b = rfc8291::encrypt_with(&salt, &ephemeral, browser_pub.as_bytes(), &auth, b"x").unwrap();
        assert_eq!(a, b);
        // but a fresh random salt/key must differ
        let c = rfc8291::encrypt(browser_pub.as_bytes(), &auth, b"x").unwrap();
        assert_ne!(a, c);
    }
}
