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
        "Bypass.yml" => {
            extract_heredoc(&script, "cat > \"${STAGE_BYPASS_CONFIG}\" <<EOF\n", "\nEOF")
        }
        "Null.yml" => extract_heredoc(&script, "cat > \"${STAGE_NULL_CONFIG}\" <<'EOF'\n", "\nEOF"),
        other => panic!("unknown template: {other}"),
    };

    serde_norway::from_str(&yaml).unwrap_or_else(|err| {
        panic!("{name} template should parse as YAML: {err}\n---\n{yaml}\n---")
    })
}

fn boot_hook_script() -> String {
    extract_heredoc(
        &installer_script(),
        "cat > \"${BUILD_DIR}/usr/local/tce.installed/${EXTENSION_NAME}\" <<'TCEEOF'\n",
        "\nTCEEOF",
    )
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
    let root = doc
        .as_object()
        .expect("Bypass.yml should parse into a top-level mapping");
    let devices = devices(&doc, "Bypass.yml");
    let capture = capture(&doc, "Bypass.yml");
    let playback = devices
        .get("playback")
        .and_then(Value::as_object)
        .expect("Bypass.yml should contain devices.playback mapping");

    assert!(
        !root.contains_key("stop_on_inactive"),
        "Bypass.yml must not put stop_on_inactive at the document root"
    );
    assert!(
        !devices.contains_key("stop_on_inactive"),
        "Bypass.yml must not put stop_on_inactive directly under devices (must be under devices.capture)"
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
    let root = doc
        .as_object()
        .expect("Null.yml should parse into a top-level mapping");
    let devices = devices(&doc, "Null.yml");
    let capture = capture(&doc, "Null.yml");
    let playback = devices
        .get("playback")
        .and_then(Value::as_object)
        .expect("Null.yml should contain devices.playback mapping");

    assert!(
        !root.contains_key("stop_on_inactive"),
        "Null.yml must not put stop_on_inactive at the document root"
    );
    assert!(
        !devices.contains_key("stop_on_inactive"),
        "Null.yml must not put stop_on_inactive directly under devices (must be under devices.capture)"
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

#[test]
fn staged_pcp_config_enables_squeezelite_autostart_when_routing_to_picorecdsp() {
    let script = installer_script();
    assert!(
        script.contains("sed 's|^SL_AUTOSTART=.*|SL_AUTOSTART=\"yes\"|' -i \"${PCP_STAGED}\""),
        "installer should force SL_AUTOSTART=yes when staging pcp.cfg"
    );
    assert!(
        script.contains("printf '%s\\n' 'SL_AUTOSTART=\"yes\"' >> \"${PCP_STAGED}\""),
        "installer should append SL_AUTOSTART=yes when pcp.cfg does not already define it"
    );
}

#[test]
fn boot_hook_waits_for_websocket_probe_before_starting_picorecdsp_daemon() {
    let boot_hook = boot_hook_script();
    let ws_check = "PICORECDSP_CAMILLA_URL=\"ws://127.0.0.1:1234\" \\\n        /usr/local/bin/picorecdsp --ws-check >/dev/null 2>&1";
    let wait_index = boot_hook
        .find(ws_check)
        .expect("boot hook should probe CamillaDSP with picorecdsp --ws-check");
    let daemon_index = boot_hook
        .find("# piCoreCDSP daemon supervisor")
        .expect("boot hook should still start the piCoreCDSP daemon supervisor");
    assert!(
        wait_index < daemon_index,
        "boot hook must wait for a successful WebSocket probe before starting piCoreCDSP"
    );
    assert!(
        boot_hook.contains("else\n    sleep 1\nfi"),
        "boot hook should leave a short settle delay after the WebSocket probe succeeds"
    );
}

#[test]
fn installer_defaults_to_skipping_websocket_smoke_test_before_reboot() {
    let script = installer_script();
    assert!(
        script.contains("CAMILLA_WS_SMOKE_TEST=\"${CAMILLA_WS_SMOKE_TEST:-false}\""),
        "installer should default CAMILLA_WS_SMOKE_TEST to false"
    );
    assert!(
        script.contains("if [ \"${CAMILLA_WS_SMOKE_TEST}\" = \"true\" ]; then"),
        "installer should only run the WebSocket smoke test when explicitly enabled"
    );
    assert!(
        script.contains("Skipping CamillaDSP WebSocket smoke test during installation."),
        "installer should make it explicit that startup checks are deferred until reboot"
    );
}

#[test]
fn camillagui_config_allows_file_playback_type_for_null_config() {
    let script = installer_script();
    // Null.yml uses `type: File` for its playback device (routes to /dev/null).
    // The generated camillagui.yml must include "File" in supported_playback_types
    // so that CamillaGUI does not mark Null.yml invalid with a red X.
    assert!(
        script.contains("  - \"File\""),
        "camillagui.yml supported_playback_types must include \"File\" so Null.yml passes validation"
    );
}

#[test]
fn installer_seeds_statefile_with_bypass_config_path_on_fresh_install() {
    let script = installer_script();
    // On a fresh install the statefile must be seeded with Bypass.yml as the
    // initial config_path so that CamillaDSP loads a config on first boot and
    // CamillaGUI shows it as the active (starred) config.
    assert!(
        script.contains(
            "printf 'config_path: \"%s\"\\n' \"${BYPASS_CONFIG}\" | sudo tee \"${STATEFILE}\" >/dev/null"
        ),
        "installer should seed the statefile with Bypass.yml on fresh install"
    );
}
