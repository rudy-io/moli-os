//! `xtask integration` — build/launch/teardown the all-clusters-app DUT, then
//! run the integration test suite.
//!
//! Usage: `cargo xtask integration` (or `just integration`).
//!
//! Steps:
//! 1. Locate (and optionally build) connectedhomeip's `all-clusters-app`.
//! 2. Kill any stale DUT process that might be holding a UDP port.
//! 3. Launch the app with a fresh temp KVS dir, redirecting output to a log
//!    file under `target/integration-dut/`.
//! 4. Wait up to 30 s for the app to log `"Server Listening"`.
//! 5. Run `cargo test -p integration-tests` with the necessary env vars set.
//! 6. Kill the app on every exit path via a Drop-based guard.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// The `GeneralDiagnostics.TestEventTrigger` enable key the DUT is launched
/// with (`--enable-key`) unless its [`EventStimulus`] is `None`. chip's
/// default key is all zeros, which disables triggers. The tests send the same
/// bytes: `TEST_EVENT_ENABLE_KEY` in `crates/integration-tests/src/events.rs`,
/// whose unit test reads this literal from source and compares the two.
const TEST_EVENT_ENABLE_KEY_HEX: &str = "00112233445566778899aabbccddeeff";

/// File name of the app's out-of-band command FIFO under the DUT dir.
const APP_PIPE_FILE: &str = "app-pipe";

/// How the event tests may stimulate events on a DUT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventStimulus {
    /// Neither: the app is launched exactly as before the event tests needed it.
    None,
    /// `--enable-key` [`TEST_EVENT_ENABLE_KEY_HEX`], so
    /// `GeneralDiagnostics.TestEventTrigger` works. Which triggers exist is
    /// the app's build-time choice (evse-app: `EnergyEvse`, DEM and energy
    /// reporting).
    TestEventTriggers,
    /// `--enable-key` plus `--app-pipe <dut_dir>/app-pipe` (exported to the
    /// tests as `MATTER_INTEGRATION_APP_PIPE`): the JSON command FIFO that
    /// all-clusters, lock-app and rvc-app read, for events with no trigger
    /// (and, on rvc-app, to reset the robot between tests).
    TriggersAndAppPipe,
}

/// A connectedhomeip example app the harness can drive as a DUT.
struct AppSpec {
    /// Canonical app name (also exported as `MATTER_INTEGRATION_DUT_APP`).
    name: &'static str,
    /// `build_examples.py` target suffix: `<host-target>-<suffix>`.
    target_suffix: &'static str,
    /// Binary name under `out/<host-target>-<suffix>/`.
    binary: &'static str,
    /// If set, this fixed setup QR is used; if `None`, the device's QR is parsed
    /// from the app log (keeps the proven all-clusters path on its known QR).
    fixed_qr: Option<&'static str>,
    /// The `--test <name>` binaries `cargo test` is restricted to, so a
    /// minimal DUT doesn't run the all-clusters cluster tests. Empty: every
    /// test binary (all-clusters).
    tests: &'static [&'static str],
    /// Extra CLI args passed to the DUT after `--KVS <path>` (default `&[]`).
    /// e.g. lit-icd-app's short ICD timers so a Check-In is prompt.
    extra_args: &'static [&'static str],
    /// If set, generate a `.ota` image before the tests and export its path as
    /// `MATTER_INTEGRATION_OTA_IMAGE` (the OTA requestor test consumes it).
    needs_ota_image: bool,
    /// On Linux, build/run the app's `-platform-mdns` target variant (chip
    /// advertises + resolves via avahi instead of its minimal-mDNS). Needed
    /// when the app must RESOLVE the controller's mdns-sd advertisement: on a
    /// headless runner there is no mediating responder, and chip's minmdns
    /// resolver does not see mdns-sd records (macOS's system mDNSResponder
    /// mediates, so the plain target works there). No effect on macOS.
    linux_platform_mdns: bool,
    /// What the event tests may use to stimulate events on this DUT.
    events: EventStimulus,
}

/// Resolve the DUT app spec from the optional `xtask integration <app>` argument.
// One match arm per chip example app the harness drives (eight with tv-app):
// splitting the table would only hide it.
#[allow(clippy::too_many_lines)]
fn app_spec(app: Option<&str>) -> Result<AppSpec, String> {
    Ok(match app.unwrap_or("all-clusters") {
        "all-clusters" => AppSpec {
            name: "all-clusters",
            target_suffix: "all-clusters",
            binary: "chip-all-clusters-app",
            fixed_qr: Some("MT:-24J042C00KA0648G00"),
            tests: &[],
            extra_args: &[],
            needs_ota_image: false,
            linux_platform_mdns: false,
            events: EventStimulus::TriggersAndAppPipe,
        },
        "lock" => AppSpec {
            name: "lock",
            target_suffix: "lock",
            binary: "chip-lock-app",
            fixed_qr: None,
            tests: &["clusters_door_lock"],
            extra_args: &[],
            needs_ota_image: false,
            linux_platform_mdns: false,
            events: EventStimulus::TriggersAndAppPipe,
        },
        "evse" => AppSpec {
            name: "evse",
            target_suffix: "evse",
            binary: "chip-evse-app",
            fixed_qr: None,
            // M9-A3 B5: EnergyEvse, DeviceEnergyManagement and PowerTopology
            // in clusters_evse, next to the Electrical* and energy-mode tests.
            tests: &["clusters_electrical", "clusters_evse"],
            extra_args: &[],
            needs_ota_image: false,
            linux_platform_mdns: false,
            events: EventStimulus::TestEventTriggers,
        },
        // M9-A3 B2: RvcRunMode / RvcCleanMode; B3: RvcOperationalState and
        // ServiceArea. The tests drive rvc-app's state machine and stimulate
        // its events over the app pipe (Reset, ErrorEvent, ActivityComplete,
        // ...; rvc-app/linux/RvcAppCommandDelegate.cpp). rvc-app ignores the
        // enable key: it has no test-event triggers.
        "rvc" => AppSpec {
            name: "rvc",
            target_suffix: "rvc",
            binary: "chip-rvc-app",
            fixed_qr: None,
            tests: &["clusters_rvc"],
            extra_args: &[],
            needs_ota_image: false,
            linux_platform_mdns: false,
            events: EventStimulus::TriggersAndAppPipe,
        },
        // M9-A3 B3: MicrowaveOvenControl, which only microwave-oven-app
        // serves. Nothing is stimulated: the tests use no events, and the app
        // reads no pipe (microwave-oven-app/linux/main.cpp), and nothing these
        // tests need is stimulated by a trigger (AppMain's test-event handlers
        // are generic ones, none for these clusters), so it gets neither
        // --enable-key nor --app-pipe.
        "microwave-oven" => AppSpec {
            name: "microwave-oven",
            target_suffix: "microwave-oven",
            binary: "chip-microwave-oven-app",
            fixed_qr: None,
            tests: &["clusters_microwave_oven"],
            extra_args: &[],
            needs_ota_image: false,
            linux_platform_mdns: false,
            events: EventStimulus::None,
        },
        // M9-A3 B5: Messages, which only tv-app serves among chip's example
        // apps; M9-A3 B7: the media clusters on tv-app's endpoint 1
        // (all-clusters serves only WakeOnLan and LowPower of them), and
        // Channel, ApplicationLauncher, ContentControl and the Content App
        // endpoints' ApplicationBasic and AccountLogin (clusters_tv_apps).
        // Nothing is stimulated: chip's Messages server logs no events, the
        // media and TV-platform tests use no event trigger (none of their
        // events has a stimulus that reaches the log; chip's server never
        // logs LoggedOut), and
        // tv-app takes no --app-pipe, so
        // it gets neither --enable-key nor --app-pipe. tv-app does run chip's
        // interactive shell on stdin (`chip_build_libshell = true` in its
        // linux/args.gni, at v1.4.2.0 and master), which exits the app on
        // EOF; spawn_dut keeps the DUT's stdin open for that reason. Like
        // rvc-app, it does not build on Apple Silicon without Rosetta 2.
        "tv" => AppSpec {
            name: "tv",
            target_suffix: "tv-app",
            binary: "chip-tv-app",
            fixed_qr: None,
            tests: &["clusters_messages", "clusters_media", "clusters_tv_apps"],
            extra_args: &[],
            needs_ota_image: false,
            linux_platform_mdns: false,
            events: EventStimulus::None,
        },
        "icd" => AppSpec {
            name: "icd",
            target_suffix: "lit-icd",
            binary: "lit-icd-app",
            fixed_qr: None,
            tests: &["icd_checkin"],
            // Short ICD timers so the device goes idle ~1s after we go quiet and
            // checks in on a 15s cycle (defaults are 10s active / 3600s idle).
            // chip requires activeModeDuration(ms) <= idleModeDuration(s).
            extra_args: &[
                "--icdActiveModeDurationMs",
                "1000",
                "--icdIdleModeDuration",
                "15",
            ],
            needs_ota_image: false,
            // The ICD flow's Check-In requires the DEVICE to resolve the
            // controller's operational advertisement — the direction that
            // needs avahi mediation on a headless Linux runner.
            linux_platform_mdns: true,
            events: EventStimulus::None,
        },
        "ota" => AppSpec {
            name: "ota",
            target_suffix: "ota-requestor",
            binary: "chip-ota-requestor-app",
            fixed_qr: None,
            tests: &["ota_flow"],
            // Without --autoApplyImage the app stops after the BDX download
            // (StateTransition -> idle) and never sends ApplyUpdateRequest /
            // NotifyUpdateApplied, so the provider-side flow can't complete.
            extra_args: &["--autoApplyImage"],
            needs_ota_image: true,
            // The requestor must resolve the CONTROLLER's provider
            // advertisement to query it — the same advertise→resolve
            // direction that needs avahi mediation on a headless Linux
            // runner (see the icd spec above).
            linux_platform_mdns: true,
            events: EventStimulus::None,
        },
        other => {
            return Err(format!(
                "unknown integration app '{other}' \
                 (expected: all-clusters, lock, evse, icd, ota, rvc, microwave-oven, tv)"
            ))
        }
    })
}

/// Run the integration orchestration end-to-end for the selected DUT app.
pub(crate) fn run(app: Option<&str>) -> Result<(), String> {
    let spec = app_spec(app)?;
    let chip_root = chip_root()?;
    let binary = locate_or_build_binary(&chip_root, &spec)?;

    kill_stale_duts();

    let dut_dir = prepare_dut_dir()?;
    let kvs_path = dut_dir.join("kvs.json");
    let log_path = dut_dir.join("app.log");

    // Fresh state each run: the app must boot UNcommissioned (so the fixture can
    // commission it), and every controller store + node-id sidecar from a prior
    // run must be cleared (else a controller would try to reconnect — or re-create
    // a fabric — against a freshly-booted app that no longer has that node).
    // `controller-b-store.bin` is the second-controller store the multi_admin
    // test creates; it must be cleared too or its stale fabric breaks the re-run.
    for stale in [
        "kvs.json",
        "controller-store.bin",
        "controller-store.tmp",
        "node-id.txt",
        "controller-b-store.bin",
        "controller-b-store.tmp",
        "test.ota",
        "ota-payload.bin",
        APP_PIPE_FILE,
    ] {
        let _ = fs::remove_file(dut_dir.join(stale));
    }

    eprintln!("integration: app  → {}", spec.name);
    eprintln!("integration: launching {}", binary.display());
    eprintln!("integration: KVS  → {}", kvs_path.display());
    eprintln!("integration: log  → {}", log_path.display());

    let app_pipe =
        (spec.events == EventStimulus::TriggersAndAppPipe).then(|| dut_dir.join(APP_PIPE_FILE));
    let args = launch_args(&spec, app_pipe.as_deref());
    // The Drop guard ensures the child is killed no matter how run() exits.
    let mut guard = spawn_dut(&binary, &kvs_path, &args, &log_path)?;

    wait_for_ready(&log_path)?;
    eprintln!("integration: DUT ready — Server Listening detected");

    // The all-clusters app keeps its known QR; other apps advertise a different
    // payload, so parse the device's real QR from its startup log.
    let setup_qr = match spec.fixed_qr {
        Some(qr) => qr.to_string(),
        None => parse_setup_qr(&log_path)?,
    };

    let multicast_if = resolve_multicast_if();

    // OTA requestor needs a real `.ota` image to download; generate it now and
    // hand its path to the test via env.
    let ota_image = if spec.needs_ota_image {
        let v2 = locate_or_build_v2_requestor(&chip_root)?;
        let img = generate_ota_image(&chip_root, &dut_dir, &v2)?;
        eprintln!("integration: OTA image → {}", img.display());
        Some(img)
    } else {
        None
    };

    let status = run_tests(
        &chip_root,
        &dut_dir,
        multicast_if,
        &spec,
        &setup_qr,
        ota_image.as_deref(),
        app_pipe.as_deref(),
    )?;

    // A DUT that died mid-run fails every later test with a misleading error
    // (no mDNS record, no pipe reader). Say so, with the tail of its log:
    // later sweeps reuse app.log, so this may be the only copy that survives.
    report_dut_exit(&mut guard, &log_path);

    // Explicit teardown before we inspect the exit status, so the process is
    // gone even on a test failure.
    guard.kill_and_wait();

    if status.success() {
        eprintln!("integration: all tests passed ✓");
        Ok(())
    } else {
        Err(format!("integration tests exited with status {status}"))
    }
}

// ---------------------------------------------------------------------------
// Locate / build the DUT binary
// ---------------------------------------------------------------------------

/// Resolve `CHIP_ROOT` and verify the directory exists.
fn chip_root() -> Result<PathBuf, String> {
    let root: PathBuf = std::env::var("CHIP_ROOT")
        .unwrap_or_else(|_| "/Users/hemanshubhojak/code/connectedhomeip".into())
        .into();
    if !root.exists() {
        return Err(format!(
            "CHIP_ROOT directory does not exist: {}\n\
             Set the CHIP_ROOT environment variable to the connectedhomeip checkout.",
            root.display()
        ));
    }
    Ok(root)
}

/// Return the host-specific build target string (e.g. `darwin-arm64`).
fn host_target() -> &'static str {
    host_target_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// The `build_examples.py` host target for an OS / CPU pair, as
/// `std::env::consts` names them: `darwin-arm64` on macOS, `linux-arm64` on
/// an aarch64 Linux host (a Raspberry Pi), `linux-x64` otherwise.
fn host_target_for(os: &str, arch: &str) -> &'static str {
    match (os, arch) {
        ("macos", _) => "darwin-arm64",
        ("linux", "aarch64") => "linux-arm64",
        _ => "linux-x64",
    }
}

/// Return the expected path to the selected app's binary. If it is missing,
/// build it first.
fn locate_or_build_binary(chip_root: &Path, spec: &AppSpec) -> Result<PathBuf, String> {
    let target = host_target();
    let suffix = if spec.linux_platform_mdns && cfg!(target_os = "linux") {
        format!("{}-platform-mdns", spec.target_suffix)
    } else {
        spec.target_suffix.to_string()
    };
    let binary = chip_root
        .join("out")
        .join(format!("{target}-{suffix}"))
        .join(spec.binary);

    if binary.exists() {
        eprintln!(
            "integration: found pre-built binary at {}",
            binary.display()
        );
        return Ok(binary);
    }

    eprintln!("integration: binary not found — building {target}-{suffix} (this takes a while)");

    let script = format!(
        "source scripts/activate.sh && \
         ./scripts/build/build_examples.py \
             --target {target}-{suffix} build"
    );

    let status = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .current_dir(chip_root)
        .status()
        .map_err(|e| format!("failed to spawn build shell: {e}"))?;

    if !status.success() {
        return Err(format!(
            "connectedhomeip build failed (exit {status})\n\
             Build command: bash -c \"{script}\"\n\
             in directory: {}",
            chip_root.display()
        ));
    }

    if !binary.exists() {
        return Err(format!(
            "build appeared to succeed but binary still missing: {}",
            binary.display()
        ));
    }

    Ok(binary)
}

/// Locate (or build) the VERSION-2 ota-requestor-app: chip's
/// `GetSoftwareVersion` is the compile-time constant
/// `CHIP_DEVICE_CONFIG_DEVICE_SOFTWARE_VERSION`, and the post-reboot
/// `NotifyUpdateApplied` is sent only when the running version equals the
/// persisted target (`ConfirmCurrentImage`) — so the image the requestor
/// boots into must BE a v2 build. No `build_examples.py` token exists for
/// the version; build directly via `gn_build_example.sh`.
fn locate_or_build_v2_requestor(chip_root: &Path) -> Result<PathBuf, String> {
    let out_dir = format!("out/{}-ota-requestor-v2", host_target());
    let binary = chip_root.join(&out_dir).join("chip-ota-requestor-app");
    if binary.exists() {
        eprintln!("integration: found v2 requestor at {}", binary.display());
        return Ok(binary);
    }

    eprintln!("integration: building v2 ota-requestor (this takes a while)");
    // On Linux the v2 image must ALSO resolve via avahi (chip_mdns="platform"),
    // matching the -platform-mdns variant the launcher binary uses: the
    // REBOOTED process re-resolves the controller's provider advertisement,
    // and minmdns cannot see mdns-sd records on a headless runner (nightly
    // run 29033957877: post-reboot "Timeout waiting for mDNS resolution").
    let mdns_arg = if cfg!(target_os = "linux") {
        " chip_mdns=\"platform\""
    } else {
        ""
    };
    let script = format!(
        "source scripts/activate.sh && \
         ./scripts/examples/gn_build_example.sh examples/ota-requestor-app/linux {out_dir} \
             'chip_device_config_device_software_version=2 \
              chip_device_config_device_software_version_string=\"2.0\"{mdns_arg}'"
    );
    let status = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .current_dir(chip_root)
        .status()
        .map_err(|e| format!("failed to spawn v2 build shell: {e}"))?;
    if !status.success() {
        return Err(format!(
            "v2 ota-requestor build failed (exit {status})\n\
             Build command: bash -c \"{script}\"\n\
             in directory: {}",
            chip_root.display()
        ));
    }
    if !binary.exists() {
        return Err(format!(
            "v2 build succeeded but binary still missing: {}",
            binary.display()
        ));
    }
    Ok(binary)
}

// ---------------------------------------------------------------------------
// Stale-process cleanup
// ---------------------------------------------------------------------------

/// Every app name [`app_spec`] accepts.
const APP_NAMES: [&str; 8] = [
    "all-clusters",
    "lock",
    "evse",
    "icd",
    "ota",
    "rvc",
    "microwave-oven",
    "tv",
];

/// chip example-app binaries the harness has no [`AppSpec`] for yet but that
/// are built on harness hosts (the Linux arm64 Pi), so a stale one
/// can still hold the DUT's port. Their `build_examples.py` targets are
/// `<host>-network-manager`, `<host>-water-heater` and
/// `<host>-fabric-bridge-rpc`: fabric-bridge builds only with the `-rpc`
/// modifier (`'fabric-bridge' does not support '<host>-fabric-bridge' due to
/// rule ONLY IF '-rpc'`), so its `AppSpec`, when one is added, needs
/// `target_suffix: "fabric-bridge-rpc"`.
const OTHER_DUT_BINARIES: [&str; 3] = [
    "matter-network-manager-app",
    "fabric-bridge-app",
    "matter-water-heater-app",
];

/// The `pkill -f` pattern [`kill_stale_duts`] uses: every [`AppSpec`]
/// binary and every [`OTHER_DUT_BINARIES`] name, as an alternation. The
/// names hold only `[a-z0-9-]`, so none needs escaping. (The old pattern,
/// `chip-.*-app`, missed `lit-icd-app` and the three names above.)
fn stale_dut_pattern() -> String {
    let mut names: Vec<&str> = APP_NAMES
        .iter()
        .filter_map(|app| app_spec(Some(app)).ok())
        .map(|spec| spec.binary)
        .collect();
    names.extend(OTHER_DUT_BINARIES);
    names.join("|")
}

/// Best-effort kill of any running DUT processes (e.g. a DUT left over from a
/// previous crashed run), matched by [`stale_dut_pattern`]. pkill returns
/// non-zero when nothing matched, so we silently ignore the exit status.
fn kill_stale_duts() {
    let _ = Command::new("pkill")
        .args(["-f", &stale_dut_pattern()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // Give the OS a moment to reclaim the UDP port.
    thread::sleep(Duration::from_millis(300));
}

// ---------------------------------------------------------------------------
// Temp KVS / log directory
// ---------------------------------------------------------------------------

/// Create (and return) `<workspace>/target/integration-dut/`.
fn prepare_dut_dir() -> Result<PathBuf, String> {
    // Derive the workspace root from CARGO_MANIFEST_DIR (set when run via
    // `cargo xtask`); the dir is `xtask/`, its parent is the workspace root.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| "CARGO_MANIFEST_DIR not set; run via `cargo xtask integration`".to_string())?;
    let workspace_root = PathBuf::from(&manifest_dir)
        .parent()
        .ok_or_else(|| "could not derive workspace root from CARGO_MANIFEST_DIR".to_string())?
        .to_path_buf();

    let dut_dir = workspace_root.join("target").join("integration-dut");
    fs::create_dir_all(&dut_dir)
        .map_err(|e| format!("create DUT dir {}: {e}", dut_dir.display()))?;
    Ok(dut_dir)
}

// ---------------------------------------------------------------------------
// Spawn DUT
// ---------------------------------------------------------------------------

/// The DUT's command-line arguments after `--KVS <path>`: the spec's
/// `extra_args`, then `--enable-key` unless the spec's [`EventStimulus`] is
/// `None`, then `--app-pipe` when a pipe path is given.
fn launch_args(spec: &AppSpec, app_pipe: Option<&Path>) -> Vec<String> {
    let mut args: Vec<String> = spec.extra_args.iter().map(|a| (*a).to_string()).collect();
    if spec.events != EventStimulus::None {
        args.push("--enable-key".to_string());
        args.push(TEST_EVENT_ENABLE_KEY_HEX.to_string());
    }
    if let Some(pipe) = app_pipe {
        args.push("--app-pipe".to_string());
        args.push(pipe.to_string_lossy().into_owned());
    }
    args
}

/// Spawn the DUT app with the given KVS path.  stdout+stderr are
/// redirected to `log_path` so the main terminal stays readable.
///
/// stdin is a pipe whose write end the returned [`DutGuard`] holds, unwritten,
/// for the DUT's lifetime. An app built with chip's interactive shell (tv-app:
/// `chip_build_libshell = true`) reads stdin and exits on EOF, which an
/// inherited or null stdin gives it at once in a non-interactive run; an app
/// that never reads stdin is unaffected.
fn spawn_dut(
    binary: &Path,
    kvs_path: &Path,
    extra_args: &[String],
    log_path: &Path,
) -> Result<DutGuard, String> {
    let log_file = fs::File::create(log_path)
        .map_err(|e| format!("create log file {}: {e}", log_path.display()))?;
    // Two separate file handles for stdout and stderr.
    let log_file2 = log_file
        .try_clone()
        .map_err(|e| format!("clone log file handle for stderr: {e}"))?;

    let mut child = Command::new(binary)
        .arg("--KVS")
        .arg(kvs_path)
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file2))
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", binary.display()))?;
    // Taken out of the Child so that nothing (Child::wait closes the stdin it
    // still owns) closes it before the guard does.
    let stdin = child.stdin.take();
    let mut guard = DutGuard { child, stdin };
    if guard.stdin.is_none() {
        guard.kill_and_wait();
        return Err(format!("spawn {}: no stdin pipe", binary.display()));
    }
    Ok(guard)
}

/// Generate a `.ota` image under `dut_dir` with chip's image tool and return
/// its path. Header: vendor 0xFFF1 / product 0x8000 / version 2 (above the
/// requestor's built-in software version 1). The payload is a trampoline
/// script that boots `v2_binary` — the version-2 requestor build — so the
/// post-reboot `NotifyUpdateApplied` leg runs for real (see
/// [`locate_or_build_v2_requestor`]).
fn generate_ota_image(
    chip_root: &Path,
    dut_dir: &Path,
    v2_binary: &Path,
) -> Result<PathBuf, String> {
    // Trampoline payload: chip's OTAImageProcessor strips the OTA header,
    // renames the payload to its image path, chmods it executable, and
    // main.cpp execv()s it with the ORIGINAL argv — so this script boots the
    // v2 binary with the same --KVS (persisted state kApplying + the rotated
    // resumption record carry over) and the rebooted app sends
    // NotifyUpdateApplied. execv preserves the pid, so the harness's
    // kill-guard and the redirected app.log still apply.
    let payload = dut_dir.join("ota-payload.bin");
    let script = format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", v2_binary.display());
    fs::write(&payload, script)
        .map_err(|e| format!("write ota payload {}: {e}", payload.display()))?;
    let image = dut_dir.join("test.ota");
    let tool = chip_root.join("src/app/ota_image_tool.py");

    let status = Command::new("python3")
        .arg(&tool)
        .args([
            "create", "-v", "0xFFF1", "-p", "0x8000", "-vn", "2", "-vs", "2.0", "-da", "sha256",
        ])
        .arg(&payload)
        .arg(&image)
        .status()
        .map_err(|e| format!("spawn ota_image_tool.py: {e}"))?;
    if !status.success() {
        return Err(format!(
            "ota_image_tool.py create failed (exit {status}); tool: {}",
            tool.display()
        ));
    }
    Ok(image)
}

// ---------------------------------------------------------------------------
// Teardown guard
// ---------------------------------------------------------------------------

/// Holds a running DUT child process and kills it when dropped.
struct DutGuard {
    child: Child,
    /// The write end of the DUT's stdin pipe ([`spawn_dut`]), held open and
    /// never written; closed only when the DUT is torn down.
    stdin: Option<ChildStdin>,
}

impl DutGuard {
    /// Close the DUT's stdin, kill the child and wait for it to exit.  Called
    /// explicitly before we inspect the test exit status so the port is free
    /// for re-use.
    fn kill_and_wait(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Print a warning (with the last 30 log lines) when the DUT exited before
/// the harness tore it down. Diagnostic only: the tests decide pass or fail.
fn report_dut_exit(guard: &mut DutGuard, log_path: &Path) {
    match guard.child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => eprintln!(
            "integration: WARNING: the DUT exited during the test run ({status}); \
             any test after that point ran without one. Last lines of {}:\n{}",
            log_path.display(),
            read_tail(log_path, 30)
        ),
        Err(e) => eprintln!("integration: could not check whether the DUT is still running: {e}"),
    }
}

impl Drop for DutGuard {
    fn drop(&mut self) {
        // Idempotent: kill() on an already-dead process is harmless.
        self.kill_and_wait();
    }
}

// ---------------------------------------------------------------------------
// Readiness probe
// ---------------------------------------------------------------------------

/// Poll `log_path` until `"Server Listening"` appears or 30 seconds elapse.
/// On timeout, returns an error that includes the last few lines of the log.
fn wait_for_ready(log_path: &Path) -> Result<(), String> {
    const TIMEOUT: Duration = Duration::from_secs(30);
    const POLL_INTERVAL: Duration = Duration::from_millis(200);
    const NEEDLE: &str = "Server Listening";

    let start = Instant::now();
    loop {
        if let Ok(mut f) = fs::File::open(log_path) {
            let mut contents = String::new();
            // read_to_string is best-effort; ignore transient errors while the
            // app is still writing.
            let _ = f.read_to_string(&mut contents);
            if contents.contains(NEEDLE) {
                return Ok(());
            }
        }

        if start.elapsed() >= TIMEOUT {
            // Include the tail of the log in the error for easier diagnosis.
            let tail = read_tail(log_path, 20);
            return Err(format!(
                "timed out waiting for DUT to print \"{NEEDLE}\" after {}s.\n\
                 Last lines of {}:\n{}",
                TIMEOUT.as_secs(),
                log_path.display(),
                tail
            ));
        }

        thread::sleep(POLL_INTERVAL);
    }
}

/// Read up to `n` lines from the end of a file (best-effort; returns empty
/// string on any I/O error so the caller can still format a useful message).
fn read_tail(path: &Path, n: usize) -> String {
    let Ok(contents) = fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = contents.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

// ---------------------------------------------------------------------------
// Interface index resolution
// ---------------------------------------------------------------------------

/// Resolve the `MATTER_MULTICAST_IF` value:
///
/// - If the env var is already set by the caller, reuse it (pass-through).
/// - Otherwise try to look up `en0`'s index via `python3` (no new deps
///   needed — Python ships with macOS).
/// - If that fails, return `None`; tests that need multicast will skip.
fn resolve_multicast_if() -> Option<u32> {
    // If the caller pre-set the variable, honor it.
    if let Ok(val) = std::env::var("MATTER_MULTICAST_IF") {
        if let Ok(idx) = val.trim().parse::<u32>() {
            return Some(idx);
        }
    }

    // Shell out to Python3 (macOS / Linux standard library). Try the
    // platform-typical primary interfaces in order: en0 (macOS), then
    // eth0 / ens5 (Linux runners). Exits non-zero (StopIteration) when none
    // exist, which maps to `None` below.
    let out = Command::new("python3")
        .args([
            "-c",
            "import socket; m = {n: i for i, n in socket.if_nameindex()}; \
             print(next(m[n] for n in ('en0', 'eth0', 'ens5') if n in m))",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let s = String::from_utf8(out.stdout).ok()?;
    s.trim().parse::<u32>().ok()
}

// ---------------------------------------------------------------------------
// Run the tests
// ---------------------------------------------------------------------------

/// Parse the device's setup QR (`MT:...`) from a line like
/// `CHIP:SVR: SetupQRCode: [MT:....]` in the app log.
fn parse_setup_qr(log_path: &Path) -> Result<String, String> {
    let log = fs::read_to_string(log_path)
        .map_err(|e| format!("reading app log for SetupQRCode: {e}"))?;
    for line in log.lines() {
        if let Some(idx) = line.find("SetupQRCode: [") {
            let rest = &line[idx + "SetupQRCode: [".len()..];
            if let Some(end) = rest.find(']') {
                let qr = rest[..end].trim().to_string();
                if qr.starts_with("MT:") {
                    eprintln!("integration: parsed SetupQRCode {qr}");
                    return Ok(qr);
                }
            }
        }
    }
    Err("SetupQRCode: [MT:...] not found in app log".to_string())
}

/// Run `cargo test -p integration-tests` with the necessary env vars.
fn run_tests(
    chip_root: &Path,
    dut_dir: &Path,
    multicast_if: Option<u32>,
    spec: &AppSpec,
    setup_qr: &str,
    ota_image: Option<&Path>,
    app_pipe: Option<&Path>,
) -> Result<ExitStatus, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let mut cmd = Command::new(cargo);
    // `--no-fail-fast`: a sweep should report every failing test binary, not
    // stop at the first one — one nightly run then shows the whole picture.
    cmd.args(["test", "-p", "integration-tests", "--no-fail-fast"]);
    // Minimal DUTs run only their own test binary (so they don't execute the
    // all-clusters cluster tests against an app that lacks those clusters).
    for test in spec.tests {
        cmd.args(["--test", test]);
    }
    cmd.args(["--", "--nocapture", "--test-threads=1"]);

    cmd.env("MATTER_INTEGRATION_DUT", setup_qr);
    cmd.env("MATTER_INTEGRATION_DUT_APP", spec.name);
    cmd.env("CHIP_ROOT", chip_root);
    // Absolute DUT-state dir so the fixture's store/sidecar paths don't depend on
    // cargo's per-package test cwd.
    cmd.env("MATTER_INTEGRATION_DUT_DIR", dut_dir);

    if let Some(pipe) = app_pipe {
        cmd.env("MATTER_INTEGRATION_APP_PIPE", pipe);
        eprintln!(
            "integration: MATTER_INTEGRATION_APP_PIPE={}",
            pipe.display()
        );
    }

    if let Some(img) = ota_image {
        cmd.env("MATTER_INTEGRATION_OTA_IMAGE", img);
        eprintln!(
            "integration: MATTER_INTEGRATION_OTA_IMAGE={}",
            img.display()
        );
    }

    if let Some(idx) = multicast_if {
        cmd.env("MATTER_MULTICAST_IF", idx.to_string());
        eprintln!("integration: MATTER_MULTICAST_IF={idx}");
    } else {
        eprintln!("integration: MATTER_MULTICAST_IF not resolved — multicast tests will skip");
    }

    // Inherit stdio so test output appears directly in the terminal.
    let status = cmd
        .status()
        .map_err(|e| format!("failed to spawn cargo test: {e}"))?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// `spawn_dut` holds the DUT's stdin open: a
    /// stand-in DUT that exits on stdin EOF, as tv-app's chip shell does,
    /// stays up while the guard lives, and exits cleanly once the guard
    /// closes the pipe.
    #[cfg(unix)]
    #[test]
    fn spawn_dut_holds_the_dut_stdin_open() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("xtask-spawn-dut-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("eof-exits-dut.sh");
        fs::write(&bin, "#!/bin/sh\ncat > /dev/null\n").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let mut guard = spawn_dut(&bin, &dir.join("kvs.json"), &[], &dir.join("app.log")).unwrap();
        thread::sleep(Duration::from_millis(500));
        let early = guard.child.try_wait().unwrap();
        assert_eq!(early, None, "the DUT saw stdin EOF while the guard held it");
        drop(guard.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = guard.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "the DUT ignored stdin EOF");
            thread::sleep(Duration::from_millis(50));
        };
        assert!(status.success(), "{status}");
        drop(guard);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn event_hosts_launch_with_the_test_event_enable_key() {
        for app in ["all-clusters", "lock", "evse", "rvc"] {
            let args = launch_args(&app_spec(Some(app)).unwrap(), None);
            let at = args
                .iter()
                .position(|a| a == "--enable-key")
                .unwrap_or_else(|| panic!("{app}: no --enable-key in {args:?}"));
            assert_eq!(args[at + 1], "00112233445566778899aabbccddeeff", "{app}");
        }
    }

    #[test]
    fn app_pipe_is_passed_only_when_given() {
        let spec = app_spec(Some("lock")).unwrap();
        assert_eq!(spec.events, EventStimulus::TriggersAndAppPipe);
        let args = launch_args(&spec, Some(Path::new("/x/app-pipe")));
        assert_eq!(args[args.len() - 2..], ["--app-pipe", "/x/app-pipe"]);
        assert!(!launch_args(&spec, None).contains(&"--app-pipe".to_string()));
    }

    #[test]
    fn rvc_host_runs_only_its_own_suite_with_the_app_pipe() {
        // `build_examples.py --target darwin-arm64-rvc` (linux-x64-rvc or
        // linux-arm64-rvc on Linux) builds out/<target>-rvc/chip-rvc-app. M9-A3 B3: the tests
        // reset rvc-app and stimulate its events over --app-pipe.
        let spec = app_spec(Some("rvc")).unwrap();
        assert_eq!(
            (spec.name, spec.target_suffix, spec.binary),
            ("rvc", "rvc", "chip-rvc-app")
        );
        assert_eq!(spec.tests, ["clusters_rvc"]);
        assert_eq!(spec.fixed_qr, None);
        assert_eq!(spec.events, EventStimulus::TriggersAndAppPipe);
        assert_eq!(
            launch_args(&spec, Some(Path::new("/x/app-pipe"))),
            [
                "--enable-key",
                "00112233445566778899aabbccddeeff",
                "--app-pipe",
                "/x/app-pipe"
            ]
        );
    }

    #[test]
    fn microwave_oven_host_runs_only_its_own_suite_with_plain_arguments() {
        // `build_examples.py --target darwin-arm64-microwave-oven`
        // (linux-x64- or linux-arm64-microwave-oven on Linux) builds
        // out/<target>-microwave-oven/chip-microwave-oven-app
        // (examples/microwave-oven-app/linux/BUILD.gn). No events, no pipe:
        // neither --enable-key nor --app-pipe.
        let spec = app_spec(Some("microwave-oven")).unwrap();
        assert_eq!(
            (spec.name, spec.target_suffix, spec.binary),
            (
                "microwave-oven",
                "microwave-oven",
                "chip-microwave-oven-app"
            )
        );
        assert_eq!(spec.tests, ["clusters_microwave_oven"]);
        assert_eq!(spec.fixed_qr, None);
        assert_eq!(spec.events, EventStimulus::None);
        assert!(!spec.needs_ota_image && !spec.linux_platform_mdns);
        assert_eq!(launch_args(&spec, None), Vec::<String>::new());
    }

    #[test]
    fn evse_host_runs_its_electrical_and_evse_suites_with_triggers() {
        // M9-A3 B5: the EnergyEvse / DEM / PowerTopology tests are their own
        // binary; evse-app runs it after the B1/B2 Electrical* and mode tests.
        let spec = app_spec(Some("evse")).unwrap();
        assert_eq!(spec.tests, ["clusters_electrical", "clusters_evse"]);
        assert_eq!(spec.events, EventStimulus::TestEventTriggers);
        assert_eq!(
            launch_args(&spec, None),
            ["--enable-key", "00112233445566778899aabbccddeeff"]
        );
        // all-clusters runs every test binary.
        assert_eq!(app_spec(None).unwrap().tests, Vec::<&str>::new());
    }

    #[test]
    fn tv_host_runs_only_its_own_suite_with_plain_arguments() {
        // `build_examples.py --target darwin-arm64-tv-app` (linux-x64-tv-app
        // or linux-arm64-tv-app on Linux) builds out/<target>-tv-app/chip-tv-app
        // (scripts/build/builders/host.py). No triggers, no pipe.
        let spec = app_spec(Some("tv")).unwrap();
        assert_eq!(
            (spec.name, spec.target_suffix, spec.binary),
            ("tv", "tv-app", "chip-tv-app")
        );
        assert_eq!(
            spec.tests,
            ["clusters_messages", "clusters_media", "clusters_tv_apps"]
        );
        assert_eq!(spec.fixed_qr, None);
        assert_eq!(spec.events, EventStimulus::None);
        assert!(!spec.needs_ota_image && !spec.linux_platform_mdns);
        assert_eq!(launch_args(&spec, None), Vec::<String>::new());
    }

    #[test]
    fn unknown_app_names_the_rvc_microwave_oven_and_tv_hosts_among_the_choices() {
        let err = app_spec(Some("nope")).err().unwrap();
        assert!(
            err.contains("rvc") && err.contains("microwave-oven") && err.contains("tv"),
            "{err}"
        );
    }

    #[test]
    fn host_target_follows_the_os_and_on_linux_the_cpu() {
        // M9-A3 B6 (Pi report): an aarch64 Linux host builds linux-arm64-*.
        assert_eq!(host_target_for("macos", "aarch64"), "darwin-arm64");
        assert_eq!(host_target_for("macos", "x86_64"), "darwin-arm64");
        assert_eq!(host_target_for("linux", "aarch64"), "linux-arm64");
        assert_eq!(host_target_for("linux", "x86_64"), "linux-x64");
        let expected = if cfg!(target_os = "macos") {
            "darwin-arm64"
        } else if cfg!(target_arch = "aarch64") {
            "linux-arm64"
        } else {
            "linux-x64"
        };
        assert_eq!(host_target(), expected);
    }

    #[test]
    fn stale_dut_pattern_names_every_dut_binary() {
        let pattern = stale_dut_pattern();
        let names: Vec<&str> = pattern.split('|').collect();
        for app in APP_NAMES {
            let binary = app_spec(Some(app)).unwrap().binary;
            assert!(names.contains(&binary), "{app}: {binary} not in {pattern}");
        }
        for binary in [
            "matter-network-manager-app",
            "fabric-bridge-app",
            "matter-water-heater-app",
            "lit-icd-app",
            "chip-all-clusters-app",
            "chip-tv-app",
        ] {
            assert!(names.contains(&binary), "{binary} not in {pattern}");
        }
        // Unescaped in an ERE: no name may carry a regex metacharacter.
        assert!(
            names.iter().all(|n| !n.is_empty()
                && n.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')),
            "{pattern}"
        );
        // Every APP_NAMES entry resolved (filter_map drops one app_spec
        // refuses), and nothing else crept in.
        assert_eq!(names.len(), APP_NAMES.len() + OTHER_DUT_BINARIES.len());
    }

    #[test]
    fn non_event_hosts_keep_their_exact_arguments() {
        // icd and ota are unchanged by B1: their extra_args only.
        let icd = launch_args(&app_spec(Some("icd")).unwrap(), None);
        assert_eq!(
            icd,
            [
                "--icdActiveModeDurationMs",
                "1000",
                "--icdIdleModeDuration",
                "15"
            ]
        );
        assert_eq!(
            launch_args(&app_spec(Some("ota")).unwrap(), None),
            ["--autoApplyImage"]
        );
    }
}
