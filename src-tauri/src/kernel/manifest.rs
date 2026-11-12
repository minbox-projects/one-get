//! Ed25519 verification of component manifests.
//!
//! A manifest is signed over the canonical JSON encoding of its object without
//! the top-level `signature` field. Canonical JSON sorts object keys at every
//! nesting level, emits no insignificant whitespace, preserves array order and
//! uses a deterministic encoding for numbers and strings. Verification rejects
//! duplicate object keys so the signed and parsed views can never diverge.

use std::fmt;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::{Map, Number, Value};

/// Maximum accepted manifest size in bytes.
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

/// Typed failure returned by canonicalization and manifest verification.
///
/// `code` is a stable identifier safe for callers and logs; `message` is a
/// human-readable description. Neither contains key material or manifest data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    pub code: String,
    pub message: String,
}

impl ManifestError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ManifestError {}

/// Serialize a JSON value as canonical JSON.
///
/// Object keys are sorted recursively, arrays keep their order and no
/// insignificant whitespace is emitted. Returns an error only for a value that
/// cannot be encoded as JSON.
pub fn canonicalize(value: &Value) -> Result<String, ManifestError> {
    let mut output = String::new();
    write_canonical(value, &mut output)?;
    Ok(output)
}

fn write_canonical(value: &Value, output: &mut String) -> Result<(), ManifestError> {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(boolean) => output.push_str(if *boolean { "true" } else { "false" }),
        Value::Number(number) => output.push_str(&number.to_string()),
        Value::String(string) => {
            let encoded = serde_json::to_string(string).map_err(|_| {
                ManifestError::new("invalid_json", "string is not encodable as JSON")
            })?;
            output.push_str(&encoded);
        }
        Value::Array(items) => {
            output.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_canonical(item, output)?;
            }
            output.push(']');
        }
        Value::Object(object) => {
            output.push('{');
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                let encoded_key = serde_json::to_string(key).map_err(|_| {
                    ManifestError::new("invalid_json", "object key is not encodable as JSON")
                })?;
                output.push_str(&encoded_key);
                output.push(':');
                if let Some(item) = object.get(key) {
                    write_canonical(item, output)?;
                }
            }
            output.push('}');
        }
    }
    Ok(())
}

/// Parse and verify a signed manifest against a hex-encoded Ed25519 public key.
///
/// On success the returned object contains every field except the top-level
/// `signature`. The input is rejected when it exceeds [`MAX_MANIFEST_BYTES`],
/// is not valid JSON, repeats an object key, has a non-object root, lacks a
/// string signature, or fails signature verification.
pub fn verify_manifest(bytes: &[u8], public_key_hex: &str) -> Result<Value, ManifestError> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::new(
            "manifest_too_large",
            format!("manifest exceeds the {MAX_MANIFEST_BYTES} byte limit"),
        ));
    }

    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let parsed = StrictValue::deserialize(&mut deserializer).map_err(map_json_error)?;
    deserializer
        .end()
        .map_err(|_| ManifestError::new("invalid_json", "manifest has trailing data"))?;

    let Value::Object(mut object) = parsed.0 else {
        return Err(ManifestError::new(
            "non_object_root",
            "manifest root must be a JSON object",
        ));
    };

    let signature_value = object.remove("signature").ok_or_else(|| {
        ManifestError::new(
            "missing_signature",
            "manifest has no top-level signature field",
        )
    })?;
    let Value::String(signature_hex) = signature_value else {
        return Err(ManifestError::new(
            "invalid_signature",
            "manifest signature must be a hex string",
        ));
    };

    let unsigned = Value::Object(object);
    let canonical = canonicalize(&unsigned)?;

    let signature_bytes = hex::decode(&signature_hex)
        .map_err(|_| ManifestError::new("invalid_signature", "signature is not valid hex"))?;
    let signature_bytes: [u8; 64] = signature_bytes.try_into().map_err(|_| {
        ManifestError::new("invalid_signature", "signature must be exactly 64 bytes")
    })?;
    let signature = Signature::from_bytes(&signature_bytes);

    let key_bytes = hex::decode(public_key_hex)
        .map_err(|_| ManifestError::new("invalid_public_key", "public key is not valid hex"))?;
    let key_bytes: [u8; 32] = key_bytes.try_into().map_err(|_| {
        ManifestError::new("invalid_public_key", "public key must be exactly 32 bytes")
    })?;
    let verifying_key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| {
        ManifestError::new(
            "invalid_public_key",
            "public key is not a valid Ed25519 point",
        )
    })?;

    verifying_key
        .verify_strict(canonical.as_bytes(), &signature)
        .map_err(|_| {
            ManifestError::new(
                "signature_mismatch",
                "manifest signature does not match the canonical content",
            )
        })?;

    Ok(unsigned)
}

fn map_json_error(error: serde_json::Error) -> ManifestError {
    if error.to_string().contains("duplicate key") {
        ManifestError::new("duplicate_key", "manifest repeats an object key")
    } else {
        ManifestError::new(
            "invalid_json",
            format!("manifest is not valid JSON: {error}"),
        )
    }
}

/// A JSON value parsed with duplicate object keys rejected at every level.
struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(|number| StrictValue(Value::Number(number)))
            .ok_or_else(|| de::Error::custom("non-finite number in JSON"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_string())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        StrictValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut items = Vec::new();
        while let Some(item) = sequence.next_element::<StrictValue>()? {
            items.push(item.0);
        }
        Ok(StrictValue(Value::Array(items)))
    }

    fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = Map::new();
        while let Some((key, value)) = access.next_entry::<String, StrictValue>()? {
            if object.insert(key.clone(), value.0).is_some() {
                return Err(de::Error::custom(format!("duplicate key `{key}`")));
            }
        }
        Ok(StrictValue(Value::Object(object)))
    }
}
