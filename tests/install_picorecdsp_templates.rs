use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn installer_script() -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("install_picorecdsp.sh");
    fs::read_to_string(path).expect("installer script should be readable")
}

fn extract_heredoc(script: &str, start_marker: &str, end_marker: &str) -> String {
    let start = script
        .find(start_marker)
        .unwrap_or_else(|| panic!("missing start marker: {start_marker}"));
    let content_start = start + start_marker.len();
    let rest = &script[content_start..];
    let end = rest
        .find(end_marker)
        .unwrap_or_else(|| panic!("missing end marker: {end_marker}"));
    rest[..end].trim().to_string()
}

fn parse_template(name: &str) -> Value {
    let script = installer_script();
    let yaml = match name {
        "Bypass.yml" => extract_heredoc(
            &script,
            "cat > \"${STAGE_BYPASS_CONFIG}\" <<EOF\n",
            "\nEOF",
        ),
        "Null.yml" => extract_heredoc(
            &script,
            "cat > \"${STAGE_NULL_CONFIG}\" <<'EOF'\n",
            "\nEOF",
        ),
        other => panic!("unknown template: {other}"),
    };

    serde_norway::from_str(&yaml).unwrap_or_else(|err| {
        panic!("{name} template should parse as YAML: {err}\n---\n{yaml}\n---")
    })
}

fn devices<'a>(doc: &'a Value, name: &str) -> &'a serde_json::Map<String, Value> {
    doc.get("devices")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{name} should contain devices mapping"))
}

fn capture<'a>(doc: &'a Value, name: &str) -> &'a serde_json::Map<String, Value> {
    devices(doc, name)
        .get("capture")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{name} should contain devices.capture mapping"))
}

#[test]
fn bypass_template_uses_capture_scoped_stop_on_inactive_and_s32_le() {
    let doc = parse_template("Bypass.yml");
    let devices = devices(&doc, "Bypass.yml");
    let capture = capture(&doc, "Bypass.yml");
    let playback = devices
        .get("playback")
        .and_then(Value::as_object)
        .expect("Bypass.yml should contain devices.playback mapping");

    assert!(
        !devices.contains_key("stop_on_inactive"),
        "Bypass.yml must not put stop_on_inactive at devices.* root"
    );
    assert_eq!(
        capture.get("stop_on_inactive"),
        Some(&Value::Bool(true)),
        "Bypass.yml must set devices.capture.stop_on_inactive to true"
    );
    assert_eq!(
        capture.get("format"),
        Some(&Value::String("S32_LE".into())),
        "Bypass.yml must use S32_LE capture format"
    );
    assert_eq!(
        playback.get("format"),
        Some(&Value::String("S32_LE".into())),
        "Bypass.yml must use S32_LE playback format"
    );
}

#[test]
fn null_template_uses_capture_scoped_stop_on_inactive_and_s32_le() {
    let doc = parse_template("Null.yml");
    let devices = devices(&doc, "Null.yml");
    let capture = capture(&doc, "Null.yml");
    let playback = devices
        .get("playback")
        .and_then(Value::as_object)
        .expect("Null.yml should contain devices.playback mapping");

    assert!(
        !devices.contains_key("stop_on_inactive"),
        "Null.yml must not put stop_on_inactive at devices.* root"
    );
    assert_eq!(
        capture.get("stop_on_inactive"),
        Some(&Value::Bool(true)),
        "Null.yml must set devices.capture.stop_on_inactive to true"
    );
    assert_eq!(
        capture.get("format"),
        Some(&Value::String("S32_LE".into())),
        "Null.yml must use S32_LE capture format"
    );
    assert_eq!(
        playback.get("format"),
        Some(&Value::String("S32_LE".into())),
        "Null.yml must use S32_LE playback format"
    );
}
