// ─── Pure text-parsing helpers (unit-testable) ─────────────────────────────

/// Extract `VmRSS: N kB` from `/proc/<pid>/status`, returning N in KiB.
pub(crate) fn parse_rss_kib(status: &str) -> Option<u64> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// Sum `voluntary_ctxt_switches` + `nonvoluntary_ctxt_switches` from
/// `/proc/<pid>/status`.
pub(crate) fn parse_context_switches(status: &str) -> Option<u64> {
    let mut voluntary: Option<u64> = None;
    let mut nonvoluntary: Option<u64> = None;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("voluntary_ctxt_switches:") {
            voluntary = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        } else if let Some(rest) = line.strip_prefix("nonvoluntary_ctxt_switches:") {
            nonvoluntary = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        }
    }
    match (voluntary, nonvoluntary) {
        (Some(v), Some(nv)) => Some(v + nv),
        (Some(v), None) => Some(v),
        (None, Some(nv)) => Some(nv),
        (None, None) => None,
    }
}

/// Extract `utime + stime` (jiffies) from `/proc/<pid>/stat` (fields 14 and 15).
///
/// The comm field may contain spaces so we locate the last `)` to skip it.
pub(crate) fn parse_cpu_jiffies(stat: &str) -> Option<u64> {
    let after_comm = stat.rfind(')')?;
    let rest = &stat[after_comm + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // Relative to the text after ')':
    //   index 0 = state
    //   index 11 = utime
    //   index 12 = stime
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// Count XRUN-related lines in `aplay` stderr/stdout output.
///
/// `aplay` writes lines like `aplay: xrun.c:380: ...` or just `XRUN` on
/// underrun events.
pub(crate) fn count_xruns_in_aplay_output(text: &str) -> u64 {
    text.lines()
        .filter(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("xrun") || l.contains("overrun") || l.contains("underrun")
        })
        .count() as u64
}

/// Extract the Raspberry Pi hardware/model string from `/proc/cpuinfo`.
pub(crate) fn parse_pi_model(cpuinfo: &str) -> String {
    for line in cpuinfo.lines() {
        for prefix in ["Model\t\t:", "Model\t:", "Hardware\t:", "Hardware:"] {
            if let Some(rest) = line.strip_prefix(prefix) {
                let val = rest.trim().to_owned();
                if !val.is_empty() {
                    return val;
                }
            }
        }
    }
    "unknown".to_owned()
}

/// Trim the first non-empty line from a piCorePlayer version file.
pub(crate) fn parse_pcp_version(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("unknown")
        .to_owned()
}

/// Derive the aloop playback PCM device from the HCTL control device name.
///
/// The player (squeezelite / aplay) writes to `hw:<card>,1,0` while the
/// controller reads HCTL events on `hw:<card>,0`.
///
/// Expects an ALSA device string with the `hw:` prefix, e.g. `hw:Loopback,0`
/// or `hw:Loopback,0,0`.  Passing a name without `hw:` (e.g. `"Loopback"`)
/// will produce a string like `"Loopback,1,0"` which is not a valid ALSA PCM
/// device — always include the `hw:` prefix.
pub(crate) fn aloop_playback_device(control_device: &str) -> String {
    let card = control_device.split(',').next().unwrap_or(control_device);
    format!("{},1,0", card)
}

/// Parse `rate: N ...` from a `/proc/asound/card*/pcm*/sub*/hw_params` file.
pub(crate) fn parse_proc_hwparams_rate(text: &str) -> Option<u32> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("rate:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// Parse `period_size: N` from a `/proc/asound/card*/pcm*/sub*/hw_params` file.
pub(crate) fn parse_proc_hwparams_period_size(text: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("period_size:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rss_kib_extracts_vmrss_line() {
        let status = "Name:\tpicoredsp-controller\nVmRSS:\t 2048 kB\nVmPeak:\t 3000 kB\n";
        assert_eq!(parse_rss_kib(status), Some(2048));
    }

    #[test]
    fn parse_rss_kib_returns_none_when_field_absent() {
        let status = "Name:\tfoo\nVmPeak:\t 3000 kB\n";
        assert_eq!(parse_rss_kib(status), None);
    }

    #[test]
    fn parse_context_switches_sums_voluntary_and_nonvoluntary() {
        let status = "voluntary_ctxt_switches:\t100\nnonvoluntary_ctxt_switches:\t25\n";
        assert_eq!(parse_context_switches(status), Some(125));
    }

    #[test]
    fn parse_context_switches_handles_missing_nonvoluntary() {
        let status = "voluntary_ctxt_switches:\t42\n";
        assert_eq!(parse_context_switches(status), Some(42));
    }

    #[test]
    fn parse_context_switches_returns_none_when_both_absent() {
        let status = "Name:\tfoo\n";
        assert_eq!(parse_context_switches(status), None);
    }

    #[test]
    fn parse_cpu_jiffies_extracts_utime_stime() {
        // Minimal /proc/<pid>/stat with a comm that does not contain spaces.
        let stat = "123 (picoredsp) S 1 123 123 0 -1 4194560 0 0 0 0 12 5 0 0 20 0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        // utime = field 14 (0-based after ')') = index 11 → 12
        // stime = field 15 → index 12 → 5
        assert_eq!(parse_cpu_jiffies(stat), Some(17));
    }

    #[test]
    fn parse_cpu_jiffies_handles_comm_with_spaces() {
        // comm = "(my prog)" — last ')' is after the comm
        let stat = "42 (my prog) S 1 42 42 0 -1 0 0 0 0 0 8 3 0 0 20 0 1 0 0 0 0";
        assert_eq!(parse_cpu_jiffies(stat), Some(11));
    }

    #[test]
    fn count_xruns_in_aplay_output_counts_xrun_lines() {
        let output = "Playing raw data '/dev/zero' : Signed 16 bit ...\n\
                      aplay: xrun.c:380: ...\n\
                      aplay: xrun.c:380: ...\n\
                      Unrelated line\n";
        assert_eq!(count_xruns_in_aplay_output(output), 2);
    }

    #[test]
    fn count_xruns_in_aplay_output_counts_overrun_and_underrun() {
        let output = "overrun!!!\nunderrun!!!\nnormal line\n";
        assert_eq!(count_xruns_in_aplay_output(output), 2);
    }

    #[test]
    fn count_xruns_in_aplay_output_zero_when_clean() {
        let output = "Playing raw data...\nDone.\n";
        assert_eq!(count_xruns_in_aplay_output(output), 0);
    }

    #[test]
    fn count_xruns_in_aplay_output_detects_verbose_xrun_lines() {
        // aplay -v emits lines like "aplay: xrun.c:380: read/write error, state = RUNNING"
        let output =
            "Playing raw data '/dev/zero' : Signed 16 bit Little Endian, Rate 44100 Hz, Stereo\n\
                      aplay: xrun.c:380: read/write error, state = RUNNING\n\
                      aplay: xrun.c:380: read/write error, state = RUNNING\n\
                      Aborted by signal Kill...\n";
        assert_eq!(count_xruns_in_aplay_output(output), 2);
    }

    #[test]
    fn parse_pi_model_extracts_model_field() {
        let cpuinfo = "processor\t: 0\nModel\t\t: Raspberry Pi 4 Model B Rev 1.4\nSerial\t\t: 00000000deadbeef\n";
        assert_eq!(parse_pi_model(cpuinfo), "Raspberry Pi 4 Model B Rev 1.4");
    }

    #[test]
    fn parse_pi_model_falls_back_to_hardware() {
        let cpuinfo = "processor\t: 0\nHardware\t: BCM2711\nRevision\t: c03114\n";
        assert_eq!(parse_pi_model(cpuinfo), "BCM2711");
    }

    #[test]
    fn parse_pi_model_returns_unknown_when_absent() {
        let cpuinfo = "processor\t: 0\n";
        assert_eq!(parse_pi_model(cpuinfo), "unknown");
    }

    #[test]
    fn parse_pcp_version_trims_first_nonempty_line() {
        let text = "\n  9.2.0  \nsome other content\n";
        assert_eq!(parse_pcp_version(text), "9.2.0");
    }

    #[test]
    fn parse_pcp_version_returns_unknown_for_empty_input() {
        assert_eq!(parse_pcp_version(""), "unknown");
    }

    #[test]
    fn aloop_playback_device_derives_playback_side() {
        assert_eq!(aloop_playback_device("hw:Loopback,0"), "hw:Loopback,1,0");
        assert_eq!(aloop_playback_device("hw:Loopback,0,0"), "hw:Loopback,1,0");
        assert_eq!(aloop_playback_device("hw:1,0"), "hw:1,1,0");
    }

    #[test]
    fn parse_proc_hwparams_rate_and_period_size() {
        let hw_params = "access: MMAP_INTERLEAVED\n\
                         format: S16_LE\n\
                         subformat: STD\n\
                         channels: 2\n\
                         rate: 44100 (44100/1)\n\
                         period_size: 1024\n\
                         buffer_size: 4096\n";
        assert_eq!(parse_proc_hwparams_rate(hw_params), Some(44100));
        assert_eq!(parse_proc_hwparams_period_size(hw_params), Some(1024));
    }

    #[test]
    fn parse_proc_hwparams_returns_none_when_fields_absent() {
        let hw_params = "state: PREPARED\n";
        assert_eq!(parse_proc_hwparams_rate(hw_params), None);
        assert_eq!(parse_proc_hwparams_period_size(hw_params), None);
    }
}
