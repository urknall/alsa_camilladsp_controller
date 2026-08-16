//! Schema-light CamillaDSP config document (roadmap §26).
//!
//! [`ConfigDocument`] wraps the raw YAML tree received from CamillaDSP as a
//! generic `serde_json::Value`. Rust only reads the small set of paths it needs:
//!
//! ```text
//! devices.samplerate
//! devices.capture_samplerate
//! devices.resampler
//! devices.capture.type
//! devices.capture.device
//! devices.capture.channels
//! devices.capture.format
//! devices.capture.stop_on_inactive
//! ```
//!
//! Everything else — filters, mixer, pipeline, processors, FIR paths — is carried
//! through untouched. Rust never writes a full config schema: it only patches the
//! rate field that needs updating.
//!
//! The `$samplerate$` token guard (roadmap §21 / Cliffhanger D) is also
//! implemented here: any config that contains `$samplerate$` in a string value
//! fails closed rather than silently producing a broken rate-patched config.

use serde_json::Value;

/// A schema-light CamillaDSP config document (roadmap §26).
///
/// The inner representation is a `serde_json::Value` tree obtained by parsing the
/// YAML config CamillaDSP returns. Rust never builds this from scratch; it only
/// reads it from `GetConfig`/`GetPreviousConfig`, patches the one rate field that
/// needs changing, and sends it back as YAML via `SetConfig`.
///
/// # Invariants
///
/// * Rust never writes user-owned fields (filters, mixer, pipeline, FIR, playback
///   device, etc.).  Those fields are carried through `with_rate` unmodified.
/// * The `$samplerate$` guard must be checked before calling `with_rate`; if the
///   guard fires, `with_rate` returns an error rather than silently corrupting the
///   config.
/// * No `runtime.yml`, no shadow config, no repair/rewrite of user YAML.
#[derive(Debug, Clone)]
pub struct ConfigDocument {
    /// The full, unparsed config tree. We use `serde_json::Value` here because it
    /// is the most convenient tree type in the Rust ecosystem and serde_norway can
    /// both read from and serialize to it.
    inner: Value,
}

/// Error produced when a [`ConfigDocument`] cannot be parsed from YAML or when a
/// path operation fails.
#[derive(Debug, Clone)]
pub struct ConfigDocumentError(pub String);

impl std::fmt::Display for ConfigDocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConfigDocument error: {}", self.0)
    }
}

impl std::error::Error for ConfigDocumentError {}

impl ConfigDocument {
    /// Parse a CamillaDSP config from raw YAML (as returned by `GetConfig` or
    /// `GetPreviousConfig`).  Returns an error if the YAML is not a mapping at the
    /// top level; inner paths are not validated here.
    pub fn from_yaml(yaml: &str) -> Result<Self, ConfigDocumentError> {
        let inner: Value =
            serde_norway::from_str(yaml).map_err(|e| ConfigDocumentError(e.to_string()))?;
        if !inner.is_object() {
            return Err(ConfigDocumentError(
                "config must be a YAML mapping at the top level".into(),
            ));
        }
        Ok(Self { inner })
    }

    /// Serialise this document back to YAML for use in a `SetConfig` call.
    pub fn to_yaml(&self) -> Result<String, ConfigDocumentError> {
        serde_norway::to_string(&self.inner).map_err(|e| ConfigDocumentError(e.to_string()))
    }

    /// Return the value at a dotted path (e.g. `"devices.samplerate"`).  Returns
    /// `None` if any component of the path is absent.
    pub fn get(&self, path: &str) -> Option<&Value> {
        let mut current = &self.inner;
        for key in path.split('.') {
            current = current.get(key)?;
        }
        Some(current)
    }

    /// Return a new `ConfigDocument` with the value at `path` replaced by `value`.
    /// Returns an error if an intermediate path component does not exist as an
    /// object, or if this document contains the `$samplerate$` token (Cliffhanger
    /// D guard — must be called before attempting any rate patch).
    pub fn with_path_value(&self, path: &str, value: Value) -> Result<Self, ConfigDocumentError> {
        // Gate: refuse to patch a config that uses $samplerate$ tokens (roadmap §21).
        if self.has_samplerate_token() {
            return Err(ConfigDocumentError(
                "$samplerate$ token detected in config; cannot safely patch the rate field \
                 — use a fixed DSP rate + resampler or separate per-rate configs as alternatives"
                    .into(),
            ));
        }
        let mut cloned = self.inner.clone();
        set_path(&mut cloned, path, value).map_err(ConfigDocumentError)?;
        Ok(Self { inner: cloned })
    }

    /// The name of the rate field that should be patched for a source-rate change,
    /// depending on whether the config has a resampler (roadmap §15).
    ///
    /// * **No resampler:** `devices.samplerate` — the DSP rate is the source rate.
    /// * **Resampler present:** `devices.capture_samplerate` — the capture
    ///   sample-rate is set to the source rate; `devices.samplerate` (the DSP output
    ///   rate) stays user-owned and is never touched.
    pub fn rate_field_path(&self) -> &'static str {
        if self.has_resampler() {
            "devices.capture_samplerate"
        } else {
            "devices.samplerate"
        }
    }

    /// Whether a resampler is configured in this document.
    pub fn has_resampler(&self) -> bool {
        // The resampler section is present when `devices.resampler` is a non-null
        // mapping.  A null or absent value means no resampler.
        match self.get("devices.resampler") {
            Some(Value::Null) | None => false,
            Some(_) => true,
        }
    }

    /// Whether any string value in this document contains the `$samplerate$` token
    /// (roadmap §21 / Cliffhanger D).
    ///
    /// Fail-closed: if this returns `true`, the caller must **not** attempt a rate
    /// patch; instead it must surface a clear error and tell the user to switch to
    /// a fixed DSP rate + resampler or to use separate per-rate config files.
    pub fn has_samplerate_token(&self) -> bool {
        value_contains_str_pattern(&self.inner, "$samplerate$")
    }

    /// Compute a cheap fingerprint of this document for race detection (roadmap
    /// §35, Cliffhanger C).  This is NOT a cryptographic hash; it is only used to
    /// detect whether the config has changed since the last read so that a stale
    /// write can be discarded and the reconciler can start over.
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // Hash the canonical JSON representation for determinism.
        let json = serde_json::to_string(&self.inner).unwrap_or_default();
        json.hash(&mut hasher);
        hasher.finish()
    }
}

/// Recursively set the value at a dotted `path` inside a `serde_json::Value` tree.
/// Returns `Err` if an intermediate component is not an object.
fn set_path(root: &mut Value, path: &str, value: Value) -> Result<(), String> {
    let mut parts = path.splitn(2, '.');
    let key = parts.next().unwrap();
    match parts.next() {
        None => {
            // Leaf: set it directly.
            match root {
                Value::Object(map) => {
                    map.insert(key.to_owned(), value);
                    Ok(())
                }
                other => Err(format!(
                    "expected an object at `{key}`, found {}",
                    other.type_str()
                )),
            }
        }
        Some(rest) => {
            // Intermediate: recurse.
            match root {
                Value::Object(map) => {
                    let child = map
                        .get_mut(key)
                        .ok_or_else(|| format!("path component `{key}` not found in config"))?;
                    set_path(child, rest, value)
                }
                other => Err(format!(
                    "expected an object at `{key}`, found {}",
                    other.type_str()
                )),
            }
        }
    }
}

/// Helper: walk a JSON value tree and return `true` if any `String` node contains
/// `pattern`.
fn value_contains_str_pattern(v: &Value, pattern: &str) -> bool {
    match v {
        Value::String(s) => s.contains(pattern),
        Value::Object(map) => map
            .values()
            .any(|child| value_contains_str_pattern(child, pattern)),
        Value::Array(arr) => arr
            .iter()
            .any(|child| value_contains_str_pattern(child, pattern)),
        _ => false,
    }
}

trait TypeStr {
    fn type_str(&self) -> &'static str;
}

impl TypeStr for Value {
    fn type_str(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_YAML: &str = r#"
devices:
  samplerate: 44100
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
"#;

    const RESAMPLER_YAML: &str = r#"
devices:
  samplerate: 96000
  capture_samplerate: 44100
  resampler:
    type: FastAsync
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
"#;

    const SAMPLERATE_TOKEN_YAML: &str = r#"
devices:
  samplerate: 44100
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
filters:
  fir_filter:
    type: Conv
    parameters:
      filename: "fir_$samplerate$.wav"
"#;

    #[test]
    fn parse_minimal_config() {
        let doc = ConfigDocument::from_yaml(MINIMAL_YAML).expect("must parse");
        assert_eq!(
            doc.get("devices.samplerate"),
            Some(&Value::Number(44100.into()))
        );
        assert_eq!(
            doc.get("devices.capture.device"),
            Some(&Value::String("hw:Loopback,0,0".into()))
        );
    }

    #[test]
    fn non_mapping_yaml_is_rejected() {
        let err = ConfigDocument::from_yaml("- a\n- b").unwrap_err();
        assert!(err.to_string().contains("mapping"));
    }

    #[test]
    fn rate_field_path_no_resampler() {
        let doc = ConfigDocument::from_yaml(MINIMAL_YAML).unwrap();
        assert_eq!(doc.rate_field_path(), "devices.samplerate");
        assert!(!doc.has_resampler());
    }

    #[test]
    fn rate_field_path_with_resampler() {
        let doc = ConfigDocument::from_yaml(RESAMPLER_YAML).unwrap();
        assert_eq!(doc.rate_field_path(), "devices.capture_samplerate");
        assert!(doc.has_resampler());
    }

    #[test]
    fn null_resampler_counts_as_no_resampler() {
        let yaml = r#"
devices:
  samplerate: 44100
  resampler: ~
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
"#;
        let doc = ConfigDocument::from_yaml(yaml).unwrap();
        assert!(!doc.has_resampler());
        assert_eq!(doc.rate_field_path(), "devices.samplerate");
    }

    #[test]
    fn with_path_value_patches_rate() {
        let doc = ConfigDocument::from_yaml(MINIMAL_YAML).unwrap();
        let patched = doc
            .with_path_value("devices.samplerate", Value::Number(96000.into()))
            .unwrap();
        assert_eq!(
            patched.get("devices.samplerate"),
            Some(&Value::Number(96000.into()))
        );
        // Other fields survive unmodified.
        assert_eq!(
            patched.get("devices.capture.format"),
            Some(&Value::String("S32_LE".into()))
        );
    }

    #[test]
    fn with_path_value_rejects_samplerate_token() {
        let doc = ConfigDocument::from_yaml(SAMPLERATE_TOKEN_YAML).unwrap();
        let err = doc
            .with_path_value("devices.samplerate", Value::Number(96000.into()))
            .unwrap_err();
        assert!(err.to_string().contains("$samplerate$"));
    }

    #[test]
    fn has_samplerate_token_detects_token_in_nested_value() {
        let doc = ConfigDocument::from_yaml(SAMPLERATE_TOKEN_YAML).unwrap();
        assert!(doc.has_samplerate_token());
    }

    #[test]
    fn clean_config_has_no_samplerate_token() {
        let doc = ConfigDocument::from_yaml(MINIMAL_YAML).unwrap();
        assert!(!doc.has_samplerate_token());
    }

    #[test]
    fn round_trip_yaml_preserves_structure() {
        let doc = ConfigDocument::from_yaml(MINIMAL_YAML).unwrap();
        let yaml = doc.to_yaml().expect("must serialise");
        let doc2 = ConfigDocument::from_yaml(&yaml).expect("must re-parse");
        assert_eq!(
            doc.get("devices.capture.channels"),
            doc2.get("devices.capture.channels")
        );
    }

    #[test]
    fn fingerprint_changes_after_patch() {
        let doc = ConfigDocument::from_yaml(MINIMAL_YAML).unwrap();
        let fp1 = doc.fingerprint();
        let patched = doc
            .with_path_value("devices.samplerate", Value::Number(96000.into()))
            .unwrap();
        let fp2 = patched.fingerprint();
        assert_ne!(fp1, fp2, "fingerprint must change after a rate patch");
    }

    #[test]
    fn missing_intermediate_path_is_an_error() {
        let doc = ConfigDocument::from_yaml(MINIMAL_YAML).unwrap();
        let err = doc
            .with_path_value("devices.nonexistent.rate", Value::Number(48000.into()))
            .unwrap_err();
        assert!(err.to_string().contains("nonexistent") || err.to_string().contains("not found"));
    }
}
