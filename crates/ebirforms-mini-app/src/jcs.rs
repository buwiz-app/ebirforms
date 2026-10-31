//! JSON Canonicalization Scheme (RFC 8785) for the objects v1 signs and
//! hashes: the manifest, session certificates and permissions.
//!
//! The Bot's broker hashes the manifest it reads (`manifest_sha256`), checks the
//! permissions digest of every app open, and verifies the certificates this
//! process signs, all over JCS bytes. The bytes must agree exactly, so this
//! follows the broker's `server/v1/jcs.mjs`: object keys sorted by UTF-16
//! code units, strings as `JSON.stringify` writes them (serde_json escapes
//! the same set, with the same lowercase `\u00xx`), and integers only. A
//! signed object never carries a fraction; one that does is refused rather
//! than trusting two float printers to agree.

use serde_json::Value;

/// The JCS text of `value`, or why it has none (a non-integer number).
pub(crate) fn canonical(value: &Value) -> Result<String, String> {
    let mut out = String::new();
    write(value, &mut out)?;
    Ok(out)
}

fn write(value: &Value, out: &mut String) -> Result<(), String> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => {
            if let Some(n) = number.as_i64() {
                safe(n.unsigned_abs())?;
                out.push_str(&n.to_string());
            } else if let Some(n) = number.as_u64() {
                safe(n)?;
                out.push_str(&n.to_string());
            } else {
                return Err(format!("signed objects carry integers only, not {number}"));
            }
        }
        Value::String(text) => out.push_str(&Value::String(text.clone()).to_string()),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write(&map[key], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// JavaScript's safe integer range, as the broker's `Number.isSafeInteger`.
fn safe(magnitude: u64) -> Result<(), String> {
    const MAX_SAFE: u64 = (1 << 53) - 1;
    if magnitude <= MAX_SAFE {
        Ok(())
    } else {
        Err(format!("{magnitude} is outside the safe integer range"))
    }
}

/// Lowercase hex SHA-256 of the JCS bytes of `value`.
pub(crate) fn sha256_hex(value: &Value) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    Ok(hex::encode(Sha256::digest(canonical(value)?.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_sort_by_utf16_and_strings_escape_like_json_stringify() {
        let value = json!({"b": 1, "a": [true, null, "x\"\n\u{1}é"], "A": {"z": -2, "y": 0}});
        assert_eq!(
            canonical(&value).unwrap(),
            r#"{"A":{"y":0,"z":-2},"a":[true,null,"x\"\n\u0001é"],"b":1}"#
        );
        // U+FF61 sorts after U+1F600's surrogates in UTF-16, before it in UTF-8.
        let value = json!({"\u{ff61}": 1, "\u{1f600}": 2});
        assert_eq!(canonical(&value).unwrap(), "{\"\u{1f600}\":2,\"\u{ff61}\":1}");
    }

    #[test]
    fn fractions_and_unsafe_integers_are_refused() {
        assert!(canonical(&json!({"x": 1.5})).is_err());
        assert!(canonical(&json!(9007199254740992u64)).is_err());
        assert_eq!(canonical(&json!(9007199254740991u64)).unwrap(), "9007199254740991");
    }

    #[test]
    fn the_empty_permissions_digest_is_the_sha256_of_braces() {
        // sha256("{}"), as the broker's permsDigest(undefined) answers.
        assert_eq!(
            sha256_hex(&json!({})).unwrap(),
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
    }
}
