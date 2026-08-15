// ─── Live system collectors ────────────────────────────────────────────────

use super::parsing::{
    aloop_playback_device, count_xruns_in_aplay_output, parse_context_switches, parse_cpu_jiffies,
    parse_pcp_version, parse_pi_model, parse_proc_hwparams_period_size, parse_proc_hwparams_rate,
    parse_rss_kib,
};
use super::report::{BenchmarkEnvironment, REQUIRED_SAMPLE_RATES_HZ};
use crate::camilladsp::websocket::{CamillaClient, CamillaWs};
use std::time::{Duration, Instant};

/// Scan `/proc/*/cmdline` for a running `picoredsp-controller --run` (daemon)
/// process and return its PID.  Returns `None` if no daemon is found.
pub(crate) fn find_controller_pid() -> Option<u32> {
    let proc_dir = std::fs::read_dir("/proc").ok()?;
    let own_pid = std::process::id();

    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let pid: u32 = match name_str.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if pid == own_pid {
            continue;
        }
        let cmdline_path = format!("/proc/{pid}/cmdline");
        let cmdline = match std::fs::read(&cmdline_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        // argv[0] is the first NUL-terminated entry.
        let argv0_end = cmdline
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(cmdline.len());
        let argv0 = std::str::from_utf8(&cmdline[..argv0_end]).unwrap_or("");
        if !argv0.ends_with("picoredsp-controller") {
            continue;
        }
        // Reject our own benchmark invocations.
        let all_args = cmdline
            .split(|&b| b == 0)
            .filter_map(|s| std::str::from_utf8(s).ok())
            .collect::<Vec<_>>()
            .join(" ");
        if !all_args.contains("--run-benchmark") && !all_args.contains("--make-benchmark") {
            return Some(pid);
        }
    }
    None
}

/// Measure the CPU usage percentage for `pid` over `interval_ms` milliseconds
/// by reading `/proc/<pid>/stat` before and after sleeping.
pub(crate) fn collect_cpu_percent(pid: u32, interval_ms: u64) -> Option<f64> {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz <= 0 {
        return None;
    }
    let hz = hz as f64;

    let stat_path = format!("/proc/{pid}/stat");
    let before = std::fs::read_to_string(&stat_path)
        .ok()
        .and_then(|s| parse_cpu_jiffies(&s))?;
    let t0 = Instant::now();
    std::thread::sleep(Duration::from_millis(interval_ms));
    let after = std::fs::read_to_string(&stat_path)
        .ok()
        .and_then(|s| parse_cpu_jiffies(&s))?;
    let elapsed_s = t0.elapsed().as_secs_f64();

    let delta = after.saturating_sub(before) as f64;
    Some((delta / hz / elapsed_s) * 100.0)
}

/// Read RSS (KiB) for `pid` from `/proc/<pid>/status`.
pub(crate) fn collect_rss_kib(pid: u32) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .as_deref()
        .and_then(parse_rss_kib)
}

/// Read total context-switch count for `pid` from `/proc/<pid>/status`.
pub(crate) fn collect_context_switches(pid: u32) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .as_deref()
        .and_then(parse_context_switches)
}

/// Find the ALSA card number from a control-device string such as
/// `hw:Loopback,0`.  Tries a numeric card index first, then searches
/// `/proc/asound/cards` for a matching card name.
pub(crate) fn find_alsa_card_number(control_device: &str) -> Option<u32> {
    let card_part = control_device.trim_start_matches("hw:").split(',').next()?;
    if let Ok(n) = card_part.parse::<u32>() {
        return Some(n);
    }
    let cards = std::fs::read_to_string("/proc/asound/cards").ok()?;
    let needle = card_part.to_ascii_lowercase();
    for line in cards.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, '[');
        let num_part = parts.next()?.trim();
        let name_part = parts.next()?.split(']').next()?.trim().to_ascii_lowercase();
        if name_part.starts_with(&needle) {
            return num_part.parse().ok();
        }
    }
    None
}

/// Read the PCM transport latency from
/// `/proc/asound/card<N>/pcm*/sub0/hw_params` while an ALSA stream is active.
/// Returns `None` if the file is absent or the stream has not been opened yet.
pub(crate) fn collect_pcm_transport_latency_ms(card_num: u32) -> Option<f64> {
    collect_pcm_transport_latency_ms_from(card_num, "/proc/asound")
}

/// Inner implementation parameterised over the `/proc/asound` base path so
/// that unit tests can point it at a temporary directory.
fn collect_pcm_transport_latency_ms_from(card_num: u32, base: &str) -> Option<f64> {
    // snd-aloop creates up to 8 subdevices per PCM stream by default.  The
    // active subdevice index is not necessarily 0 (e.g. Squeezelite may open
    // sub0 while CamillaDSP captures on sub1).  Scan sub0–sub7 for each PCM
    // direction and return the first one that reports an active rate.
    for pcm in ["pcm0p", "pcm0c", "pcm1p", "pcm1c"] {
        for sub in 0u32..8 {
            let path = format!("{base}/card{card_num}/{pcm}/sub{sub}/hw_params");
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let (Some(period), Some(rate)) = (
                    parse_proc_hwparams_period_size(&text),
                    parse_proc_hwparams_rate(&text),
                ) {
                    if rate > 0 {
                        return Some(period as f64 / rate as f64 * 1000.0);
                    }
                }
            }
        }
    }
    None
}

/// Read the configured pipeline chunk size (in frames) from CamillaDSP's
/// active config via `GetConfigValue`.
///
/// CamillaDSP 4.x has no `GetBuffersize` command; the chunk size lives in the
/// config under `devices.chunksize` and must be read with `GetConfigValue`
/// and a [JSON Pointer](https://datatracker.ietf.org/doc/html/rfc6901).
fn chunksize_from_client(client: &mut impl CamillaClient) -> Option<u64> {
    client
        .get_config_value("/devices/chunksize")
        .ok()?
        .and_then(|v| v.as_u64())
}

/// Compute the active pipeline buffer latency in milliseconds
/// (`chunksize / GetCaptureRate * 1000`) from an already-connected client.
///
/// CamillaDSP 4.x has no `GetSamplerate` command either; `GetCaptureRate`
/// returns the measured sample rate of the capture device (0 if processing
/// is not currently running).
fn buffer_latency_ms_from_client(client: &mut impl CamillaClient) -> Option<f64> {
    let rate = client.get_capture_rate().ok()?;
    let chunksize = chunksize_from_client(client)?;
    if rate == 0 {
        return None;
    }
    Some(chunksize as f64 / rate as f64 * 1000.0)
}

/// Query CamillaDSP over WebSocket for the active pipeline buffer latency in
/// milliseconds (`GetConfigValue("/devices/chunksize") / GetCaptureRate * 1000`).
///
/// Returns `None` if CamillaDSP is unreachable or not currently processing.
pub(crate) fn collect_cdsp_buffer_latency_ms(host: &str, port: u16) -> Option<f64> {
    let mut client = CamillaWs::connect(host, port).ok()?;
    let result = buffer_latency_ms_from_client(&mut client);
    client.close();
    result
}

/// Query CamillaDSP for its measured capture sample rate via `GetCaptureRate`.
/// Returns `None` on failure; callers should also treat a returned `0` (rate
/// not yet measured, e.g. processing hasn't started) as "unknown".
pub(crate) fn collect_cdsp_rate(host: &str, port: u16) -> Option<u64> {
    let mut client = CamillaWs::connect(host, port).ok()?;
    let rate = client.get_capture_rate().ok();
    client.close();
    rate
}

/// Query CamillaDSP for its version string.  Returns `"unknown"` on failure.
fn collect_cdsp_version(host: &str, port: u16) -> String {
    CamillaWs::connect(host, port)
        .ok()
        .and_then(|mut c| {
            let v = c.get_version().ok();
            c.close();
            v
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Spawn a short `aplay` test through the aloop playback device, timing how
/// long it takes for the loopback HCTL to report `active = true` (start
/// latency) and `active = false` after the process is killed (stop latency).
///
/// If another player (e.g. Squeezelite) already has the loopback active when
/// this function is called, `aplay` cannot open the same subdevice.  In that
/// case the function returns start latency as `None` and stop latency as
/// `None` — both are only meaningful when no other player holds the device.
///
/// Returns `(start_latency_ms, stop_latency_ms, xrun_count)`.
/// All are `None` / 0 if ALSA or `aplay` is unavailable.
pub(crate) fn collect_aloop_timings(control_device: &str) -> (Option<f64>, Option<f64>, u64) {
    use crate::camilladsp::alsa_capture::AlsaLoopbackListener;
    use crate::core::logging::LogLevel;

    let listener = match AlsaLoopbackListener::new(control_device, LogLevel::Error) {
        Ok(l) => l,
        Err(_) => return (None, None, 0),
    };

    // If a player is already active the loopback write side may be held open
    // (EBUSY).  Detect this before spawning aplay: if active is already true,
    // our timing measurements would be unreliable (start would read near-zero
    // because the snapshot is already active, and stop would never arrive
    // because the other player keeps the device open after we kill aplay).
    let already_active = listener.read_snapshot().map(|s| s.active).unwrap_or(false);
    if already_active {
        return (None, None, 0);
    }

    let playback_dev = aloop_playback_device(control_device);

    // Pass `-v` so that ALSA XRUN events appear in stderr output.
    let mut child = match std::process::Command::new("aplay")
        .args([
            "-D",
            &playback_dev,
            "-r",
            "44100",
            "-c",
            "2",
            "-f",
            "S16_LE",
            "-d",
            "5",
            "-v",
            "/dev/zero",
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return (None, None, 0),
    };

    // Poll for active = true (playback start latency).
    let t0 = Instant::now();
    let start_latency_ms = loop {
        if t0.elapsed() > Duration::from_secs(3) {
            break None;
        }
        if listener.read_snapshot().map(|s| s.active).unwrap_or(false) {
            break Some(t0.elapsed().as_secs_f64() * 1000.0);
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    // Let it play for 1 second so any xruns can accumulate.
    std::thread::sleep(Duration::from_secs(1));

    // Kill aplay.  Start the stop-latency clock before waiting for the process
    // to exit so that t1 begins as close to the kill signal as possible.
    let _ = child.kill();
    let t1 = Instant::now();
    let xrun_count = match child.wait_with_output() {
        Ok(out) => count_xruns_in_aplay_output(&String::from_utf8_lossy(&out.stderr)),
        Err(_) => 0,
    };

    // Poll for active = false (playback stop latency).
    let stop_latency_ms = loop {
        if t1.elapsed() > Duration::from_secs(3) {
            break None;
        }
        if !listener.read_snapshot().map(|s| s.active).unwrap_or(true) {
            break Some(t1.elapsed().as_secs_f64() * 1000.0);
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    (start_latency_ms, stop_latency_ms, xrun_count)
}

// ─── Environment auto-detection ───────────────────────────────────────────

/// Auto-detect the benchmark environment from system files and a live
/// CamillaDSP WebSocket connection.
pub(crate) fn detect_environment(
    host: &str,
    port: u16,
    aloop_device: &str,
) -> BenchmarkEnvironment {
    let raspberry_pi = std::fs::read_to_string("/proc/cpuinfo")
        .map(|s| parse_pi_model(&s))
        .unwrap_or_else(|_| "unknown".to_owned());

    let picoreplayer_version = ["/usr/local/pcp_version", "/etc/pcp_version"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| parse_pcp_version(&s))
        .unwrap_or_else(|| "unknown".to_owned());

    let camilladsp_version = collect_cdsp_version(host, port);

    let dac = std::fs::read_to_string("/proc/asound/cards")
        .map(|text| detect_dac_from_cards(&text, aloop_device))
        .unwrap_or_else(|_| "unknown".to_owned());

    // Read chunksize from CamillaDSP if available; default to 1024.
    let chunksize = {
        let chunksize_val = CamillaWs::connect(host, port).ok().and_then(|mut c| {
            let v = chunksize_from_client(&mut c);
            c.close();
            v
        });
        chunksize_val.map(|n| n as u32).unwrap_or(1024)
    };

    BenchmarkEnvironment {
        raspberry_pi,
        picoreplayer_version,
        camilladsp_version,
        dac,
        dsp_config: "auto-detected (see CamillaDSP active config path)".to_owned(),
        track: "silence via aplay /dev/zero (automated benchmark)".to_owned(),
        chunksize,
        queuelimit: 4,
        sample_rates_hz: REQUIRED_SAMPLE_RATES_HZ.to_vec(),
    }
}

/// Return the first non-loopback ALSA card description from `/proc/asound/cards`.
fn detect_dac_from_cards(cards_text: &str, aloop_device: &str) -> String {
    let loopback_name = aloop_device
        .trim_start_matches("hw:")
        .split(',')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    for line in cards_text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Lines look like: " 0 [Loopback       ]: Loopback - Loopback"
        if let Some(bracket_pos) = line.find('[') {
            let after_open = &line[bracket_pos + 1..];
            let name_end = after_open.find(']').unwrap_or(after_open.len());
            let name = after_open[..name_end].trim().to_ascii_lowercase();
            if !name.starts_with(&loopback_name) {
                // Return the description part after the ':'
                if let Some(desc_pos) = line.find(':') {
                    return line[desc_pos + 1..].trim().to_owned();
                }
                return name;
            }
        }
    }
    "unknown".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camilladsp::websocket::WsError;
    use serde_json::Value as JsonValue;
    use std::collections::VecDeque;

    /// Minimal scripted [`CamillaClient`] for testing the `collect_cdsp_*`
    /// helper functions without a live CamillaDSP process.  Verifies the
    /// exact command names sent, guarding against regressions back to the
    /// invalid `GetSamplerate`/`GetBuffersize` calls CamillaDSP 4.x rejects.
    struct MockClient {
        responses: VecDeque<Result<Option<JsonValue>, WsError>>,
        commands_sent: Vec<String>,
    }

    impl MockClient {
        fn new(responses: Vec<Result<Option<JsonValue>, WsError>>) -> Self {
            Self {
                responses: responses.into(),
                commands_sent: Vec::new(),
            }
        }
    }

    impl CamillaClient for MockClient {
        fn query(
            &mut self,
            command: &str,
            _argument: Option<JsonValue>,
        ) -> Result<Option<JsonValue>, WsError> {
            self.commands_sent.push(command.to_owned());
            self.responses
                .pop_front()
                .unwrap_or_else(|| Err(WsError::Transport("no more responses".to_owned())))
        }
    }

    #[test]
    fn find_alsa_card_number_parses_numeric_card_index() {
        // "hw:1,0" → card 1 (no /proc lookup needed)
        assert_eq!(find_alsa_card_number("hw:1,0"), Some(1));
        assert_eq!(find_alsa_card_number("hw:0,0"), Some(0));
    }

    #[test]
    fn detect_dac_from_cards_skips_loopback_returns_first_other() {
        let cards = " 0 [Loopback       ]: Loopback - Loopback\n \
                     1 [DAC            ]: USB-Audio - My DAC\n";
        let dac = detect_dac_from_cards(cards, "hw:Loopback,0");
        assert!(
            dac.contains("USB-Audio") || dac.contains("My DAC"),
            "got: {dac}"
        );
    }

    #[test]
    fn detect_dac_from_cards_returns_unknown_when_only_loopback_present() {
        let cards = " 0 [Loopback       ]: Loopback - Loopback\n";
        let dac = detect_dac_from_cards(cards, "hw:Loopback,0");
        assert_eq!(dac, "unknown");
    }

    #[test]
    fn collect_pcm_transport_scans_multiple_subdevices() {
        // Build a /proc/asound-like tree under /tmp where only sub2 is active.
        // Confirm collect_pcm_transport_latency_ms_from returns Some (the loop
        // reaches sub2) rather than None (which would happen with the old
        // sub0-only code).
        let base = format!("/tmp/pcm_transport_test_{}", std::process::id());
        let card_dir = format!("{base}/card5/pcm0p");
        for sub in 0u32..4 {
            let sub_dir = format!("{card_dir}/sub{sub}");
            std::fs::create_dir_all(&sub_dir).unwrap();
            let content = if sub == 2 {
                "access: MMAP_INTERLEAVED\n\
                 format: S16_LE\n\
                 channels: 2\n\
                 rate: 48000 (48000/1)\n\
                 period_size: 512\n\
                 buffer_size: 4096\n"
            } else {
                "closed\n"
            };
            std::fs::write(format!("{sub_dir}/hw_params"), content).unwrap();
        }

        let result = collect_pcm_transport_latency_ms_from(5, &base);
        // Clean up before asserting so temp files are always removed.
        let _ = std::fs::remove_dir_all(&base);

        // 512 / 48000 * 1000 ≈ 10.666 ms
        let ms = result.expect("should have found rate on sub2");
        assert!((ms - 10.666).abs() < 0.01, "got {ms}");
    }

    #[test]
    fn chunksize_from_client_sends_get_config_value_with_chunksize_pointer() {
        let mut client = MockClient::new(vec![Ok(Some(JsonValue::from(2048u64)))]);
        let chunksize = chunksize_from_client(&mut client);
        assert_eq!(chunksize, Some(2048));
        assert_eq!(client.commands_sent, vec!["GetConfigValue".to_owned()]);
    }

    #[test]
    fn chunksize_from_client_returns_none_on_transport_error() {
        let mut client = MockClient::new(vec![Err(WsError::Transport("down".to_owned()))]);
        assert_eq!(chunksize_from_client(&mut client), None);
    }

    #[test]
    fn buffer_latency_ms_from_client_uses_get_capture_rate_and_chunksize() {
        let mut client = MockClient::new(vec![
            Ok(Some(JsonValue::from(48_000u64))), // GetCaptureRate
            Ok(Some(JsonValue::from(1024u64))),   // GetConfigValue chunksize
        ]);
        let ms = buffer_latency_ms_from_client(&mut client).expect("should compute latency");
        // 1024 / 48000 * 1000 ≈ 21.33 ms
        assert!((ms - 21.333).abs() < 0.01, "got {ms}");
        assert_eq!(
            client.commands_sent,
            vec!["GetCaptureRate".to_owned(), "GetConfigValue".to_owned()]
        );
    }

    #[test]
    fn buffer_latency_ms_from_client_returns_none_when_rate_is_zero() {
        // CamillaDSP reports rate 0 when processing hasn't started yet.
        let mut client = MockClient::new(vec![Ok(Some(JsonValue::from(0u64)))]);
        assert_eq!(buffer_latency_ms_from_client(&mut client), None);
    }

    #[test]
    fn buffer_latency_ms_from_client_returns_none_when_get_capture_rate_errors() {
        let mut client = MockClient::new(vec![Err(WsError::Transport("down".to_owned()))]);
        assert_eq!(buffer_latency_ms_from_client(&mut client), None);
    }

    #[test]
    fn buffer_latency_ms_from_client_returns_none_when_chunksize_query_fails() {
        let mut client = MockClient::new(vec![
            Ok(Some(JsonValue::from(48_000u64))),       // GetCaptureRate
            Err(WsError::Transport("down".to_owned())), // GetConfigValue fails
        ]);
        assert_eq!(buffer_latency_ms_from_client(&mut client), None);
    }

    /// Regression test for the benchmark WebSocket API drift this module's
    /// doc comments warn about: `GetSamplerate`/`GetBuffersize` do not exist
    /// in CamillaDSP 4.x and would silently return errors/`None`. Unlike the
    /// `MockClient` tests above (which only prove *which commands* are
    /// sent), this test exercises `collect_cdsp_rate`,
    /// `collect_cdsp_buffer_latency_ms`, and `collect_cdsp_version` against
    /// a real, running CamillaDSP process to prove the currently-used
    /// `GetCaptureRate`/`GetConfigValue`/`GetVersion` commands actually work
    /// against the pinned upstream binary.
    #[test]
    #[ignore = "requires PICOREDSP_TEST_CAMILLADSP_BIN pointing at a real CamillaDSP binary"]
    fn live_collectors_work_against_real_camilladsp() {
        use crate::test_support::live_camilladsp_binary;
        use std::io::Write;
        use std::process::{Command, Stdio};

        let Some(cdsp) = live_camilladsp_binary() else {
            return;
        };

        let dir =
            std::env::temp_dir().join(format!("picoredsp-live-collectors-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.yml");
        let out_path = dir.join("out.raw");
        std::fs::write(
            &config_path,
            format!(
                "devices:\n  samplerate: 44100\n  chunksize: 1024\n  \
                 capture:\n    type: Stdin\n    channels: 2\n    format: S16_LE\n  \
                 playback:\n    type: File\n    channels: 2\n    filename: \"{}\"\n    \
                 format: S16_LE\nfilters: {{}}\nmixers: {{}}\npipeline: []\nprocessors: {{}}\n",
                out_path.display()
            ),
        )
        .unwrap();

        let port = 15551u16;
        let mut child = Command::new(&cdsp)
            .arg("-p")
            .arg(port.to_string())
            .arg("-a")
            .arg("127.0.0.1")
            .arg(&config_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn real CamillaDSP binary");

        let mut stdin = child.stdin.take().unwrap();
        let feeder = std::thread::spawn(move || {
            let silence = vec![0u8; 4096];
            for _ in 0..700 {
                if stdin.write_all(&silence).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });

        // Give CamillaDSP time to accept the WebSocket connection, then poll
        // GetCaptureRate until it reports a measured rate rather than 0
        // ("not currently processing") — CamillaDSP's rate estimator needs a
        // few chunks to settle, and a single fixed sleep is flaky under CI
        // load.
        std::thread::sleep(Duration::from_millis(300));
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut rate = None;
        while Instant::now() < deadline {
            rate = collect_cdsp_rate("127.0.0.1", port);
            if rate.is_some_and(|r| r > 0) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let latency_ms = collect_cdsp_buffer_latency_ms("127.0.0.1", port);
        let version = collect_cdsp_version("127.0.0.1", port);

        let _ = child.kill();
        let _ = child.wait();
        let _ = feeder.join();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            rate.is_some_and(|r| r > 0),
            "collect_cdsp_rate returned {rate:?} against a live CamillaDSP \
             process — GetCaptureRate may have drifted from the real \
             CamillaDSP 4.x API"
        );
        assert!(
            latency_ms.is_some(),
            "collect_cdsp_buffer_latency_ms returned None against a live \
             CamillaDSP process — GetConfigValue or GetCaptureRate may have \
             drifted from the real CamillaDSP 4.x API"
        );
        assert_ne!(
            version, "unknown",
            "collect_cdsp_version returned 'unknown' against a live CamillaDSP process"
        );
    }
}
