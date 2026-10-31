//! The plugin key the Bot provisions (Mini App Protocol v1, spec 4.3) and
//! the session certificates it signs (4.4).
//!
//! The Bot generates one ed25519 key per install and sends its seed as the
//! first request of every spawn (`rox.provision`). The seed lives in this
//! struct and nowhere else: it is never written, logged, echoed or put in an
//! error message (P4), and `Debug` is not derived so a stray `{:?}` cannot
//! print it. The key signs certificates only; bridge calls are signed by the
//! Bot's session keys and checked by its broker before they reach this
//! process.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Map, Value, json};

use crate::jcs;

/// The longest session a certificate may cover: 8 hours (Q13).
pub(crate) const CERT_MAX_MS: i64 = 8 * 3600 * 1000;

/// base64url without padding.
pub(crate) fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Exactly `len` bytes of base64url without padding, else `None`.
pub(crate) fn from_b64url(text: &str, len: usize) -> Option<Vec<u8>> {
    let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
    (bytes.len() == len && b64url(&bytes) == text).then_some(bytes)
}

/// The provisioned plugin key.
pub(crate) struct PluginKey {
    signing: SigningKey,
}

impl PluginKey {
    /// `rox.provision`'s `key` and `pub`: the seed must be 32 bytes and
    /// `pub` its public key. The error never quotes either.
    pub(crate) fn from_provision(arguments: &Map<String, Value>) -> Result<Self, String> {
        let key = arguments
            .get("key")
            .and_then(Value::as_object)
            .ok_or("`key` must be {alg, seed}")?;
        if key.get("alg").and_then(Value::as_str) != Some("ed25519") {
            return Err("`key.alg` must be ed25519".into());
        }
        let seed = key
            .get("seed")
            .and_then(Value::as_str)
            .and_then(|seed| from_b64url(seed, 32))
            .ok_or("`key.seed` must be 32 bytes of base64url")?;
        let seed: [u8; 32] = seed.try_into().map_err(|_| "`key.seed` must be 32 bytes")?;
        let signing = SigningKey::from_bytes(&seed);
        let public = b64url(signing.verifying_key().as_bytes());
        if arguments.get("pub").and_then(Value::as_str) != Some(public.as_str()) {
            return Err("the key and pub do not match".into());
        }
        Ok(Self { signing })
    }

    /// Whether `other` holds the same key (a repeated provision is fine).
    pub(crate) fn same_as(&self, other: &PluginKey) -> bool {
        self.signing.to_bytes() == other.signing.to_bytes()
    }

    /// base64url public key.
    #[cfg(test)]
    pub(crate) fn public(&self) -> String {
        b64url(self.signing.verifying_key().as_bytes())
    }

    /// base64url signature over the JCS bytes of `value`.
    pub(crate) fn sign(&self, value: &Value) -> Result<String, String> {
        let bytes = jcs::canonical(value)?;
        Ok(b64url(&self.signing.sign(bytes.as_bytes()).to_bytes()))
    }
}

/// `rox.session.open`'s request, checked: the certificate fields.
pub(crate) struct OpenRequest {
    pub app: String,
    pub open_id: String,
    pub ks_pub: String,
    pub perms_sha256: String,
    pub not_after: i64,
}

impl OpenRequest {
    pub(crate) fn read(arguments: &Map<String, Value>, now_ms: i64) -> Result<Self, String> {
        let text = |key: &str| -> Result<String, String> {
            arguments
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("`{key}` must be a string"))
        };
        let open_id = text("open_id")?;
        if from_b64url(&open_id, 16).is_none() {
            return Err("`open_id` must be 128 bits of base64url".into());
        }
        let ks_pub = text("ks_pub")?;
        if from_b64url(&ks_pub, 32).is_none() {
            return Err("`ks_pub` must be a 32-byte ed25519 key in base64url".into());
        }
        let perms_sha256 = text("perms_sha256")?;
        let not_after = arguments
            .get("not_after")
            .and_then(Value::as_i64)
            .ok_or("`not_after` must be an integer (unix ms)")?;
        if not_after <= now_ms || not_after > now_ms + CERT_MAX_MS {
            return Err("not_after must be within 8 h".into());
        }
        Ok(Self {
            app: text("app")?,
            open_id,
            ks_pub,
            perms_sha256,
            not_after,
        })
    }

    /// The certificate of spec 4.4 for `plugin`.
    pub(crate) fn certificate(&self, plugin: &str) -> Value {
        json!({
            "v": 1,
            "typ": "rox-session",
            "plugin": plugin,
            "app": self.app,
            "open_id": self.open_id,
            "ks_pub": self.ks_pub,
            "perms_sha256": self.perms_sha256,
            "not_after": self.not_after,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};

    /// A provision request for a fixed test seed (never a real key).
    pub(crate) fn provision_args(seed: [u8; 32]) -> Map<String, Value> {
        let public = b64url(SigningKey::from_bytes(&seed).verifying_key().as_bytes());
        json!({
            "protocol": "1.0.0",
            "plugin": "dev.goldcoders.bir",
            "key": {"alg": "ed25519", "seed": b64url(&seed)},
            "pub": public,
        })
        .as_object()
        .unwrap()
        .clone()
    }

    pub(crate) fn verify(public: &str, value: &Value, signature: &str) -> bool {
        let key: [u8; 32] = from_b64url(public, 32).unwrap().try_into().unwrap();
        let sig: [u8; 64] = from_b64url(signature, 64).unwrap().try_into().unwrap();
        VerifyingKey::from_bytes(&key)
            .unwrap()
            .verify(
                jcs::canonical(value).unwrap().as_bytes(),
                &Signature::from_bytes(&sig),
            )
            .is_ok()
    }

    #[test]
    fn a_provisioned_key_signs_what_its_public_key_verifies() {
        let key = PluginKey::from_provision(&provision_args([7; 32])).unwrap();
        let cert = json!({"v": 1, "typ": "rox-session", "not_after": 5});
        let signature = key.sign(&cert).unwrap();
        assert!(verify(&key.public(), &cert, &signature));
        assert!(!verify(&key.public(), &json!({"v": 2}), &signature));
    }

    #[test]
    fn a_mismatched_pub_or_bad_seed_is_refused_without_quoting_the_seed() {
        let mut args = provision_args([7; 32]);
        args.insert("pub".into(), json!(b64url(&[1; 32])));
        let error = PluginKey::from_provision(&args).err().unwrap();
        assert_eq!(error, "the key and pub do not match");
        let mut args = provision_args([7; 32]);
        args["key"]["seed"] = json!("short");
        assert!(PluginKey::from_provision(&args).is_err());
        let seed = b64url(&[7; 32]);
        let mut args = provision_args([7; 32]);
        args["key"]["alg"] = json!("rsa");
        assert!(!PluginKey::from_provision(&args).err().unwrap().contains(&seed));
    }

    #[test]
    fn open_requests_hold_not_after_to_eight_hours() {
        let base = json!({
            "app": "forms", "open_id": b64url(&[1; 16]), "ks_pub": b64url(&[2; 32]),
            "perms_sha256": "0".repeat(64), "not_after": 1_000 + CERT_MAX_MS,
        });
        let args = base.as_object().unwrap();
        assert!(OpenRequest::read(args, 1_000).is_ok());
        assert!(OpenRequest::read(args, 999).is_err());
        let mut late = args.clone();
        late.insert("open_id".into(), json!("nope"));
        assert!(OpenRequest::read(&late, 1_000).is_err());
    }
}
