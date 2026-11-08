//! Driver integration tests against a fake serial port.
//!
//! A pty (pseudo-terminal) stands in for the USB-TIC interface: the driver
//! opens the slave side exactly as it would `/dev/ttyUSB0`, while a feeder
//! thread replays a real captured stream on the master side. This exercises
//! the full driver path — port open, retry/backoff while the port is absent,
//! blocking reader, frame parsing, point publication and availability —
//! without any hardware. The pty does not emulate parity, so the raw bytes
//! of the capture pass through untouched (the driver's 7E1 termios only
//! matters on a real UART).

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use moli_core::{DeviceId, DriverStatus, InstanceId, PointId, Value};
use moli_runtime::{Hub, HubOptions, spawn_driver};
use moli_teleinfo::{Config, Teleinfo};
use nix::fcntl::OFlag;
use nix::pty::{PtyMaster, grantpt, posix_openpt, ptsname_r, unlockpt};
use tokio_util::sync::CancellationToken;

/// The raw capture (standard mode, 9600) replayed through the fake port.
fn standard_capture() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../integrations/teleinfo/fixtures/standard_raw.txt");
    fs::read(path).expect("standard capture fixture")
}

/// Driver options aimed at a fake port; every other field keeps its default
/// (9600 baud, id "linky", offline after 90 s).
fn config_with_port(port: String) -> Config {
    Config {
        port,
        baud: 9600,
        id: "linky".into(),
        name: "Linky".into(),
        offline_after: 90,
    }
}

/// Opens a fresh pty pair: the test feeds the master, and `ptsname_r` gives
/// the slave path the driver will open. `unlockpt` is mandatory — the
/// kernel keeps the slave locked (open fails with EIO) until it is called.
fn open_pty() -> (PtyMaster, String) {
    let master = posix_openpt(OFlag::O_RDWR | OFlag::O_NOCTTY).expect("posix_openpt");
    grantpt(&master).expect("grantpt");
    unlockpt(&master).expect("unlockpt");
    let slave_path = ptsname_r(&master).expect("ptsname_r");
    (master, slave_path)
}

/// Replays `data` on the pty master in 512-byte chunks, the way the meter
/// pushes its stream. Runs on its own thread so a stuck driver cannot
/// deadlock the test; the master fd is closed when the thread exits.
fn feed(mut master: PtyMaster, data: Vec<u8>) -> thread::JoinHandle<std::io::Result<()>> {
    thread::spawn(move || {
        for chunk in data.chunks(512) {
            master.write_all(chunk)?;
            thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    })
}

fn value_of(hub: &Hub, point: &str) -> Option<Value> {
    hub.state(&PointId::from(point)).map(|sample| sample.value)
}

fn driver_status(hub: &Hub, instance: &str) -> DriverStatus {
    hub.snapshot()
        .drivers
        .into_iter()
        .find(|driver| driver.instance.as_str() == instance)
        .map_or(DriverStatus::Stopped, |driver| driver.status)
}

/// True once the driver's open attempt succeeded (its fd shows up in
/// /proc/self/fd, pointing at the slave path).
fn port_is_open(slave_path: &str) -> bool {
    let Ok(entries) = fs::read_dir("/proc/self/fd") else {
        return false;
    };
    entries.filter_map(std::result::Result::ok).any(|entry| {
        fs::read_link(entry.path()).is_ok_and(|target| target == Path::new(slave_path))
    })
}

#[tokio::test]
async fn the_driver_publishes_frames_from_a_fake_serial_port() {
    let (master, slave_path) = open_pty();
    let data = standard_capture();

    let hub = Hub::new(HubOptions::default()).expect("hub");
    let cancel = CancellationToken::new();
    let driver = Arc::new(Teleinfo::new(config_with_port(slave_path.clone())).expect("config"));
    let handle = spawn_driver(&hub, InstanceId::from("teleinfo"), driver, cancel.clone());

    // The driver must open the port before feeding is meaningful.
    let open_deadline = Instant::now() + Duration::from_secs(5);
    while !port_is_open(&slave_path) {
        assert!(
            Instant::now() < open_deadline,
            "driver never opened {slave_path} (status {:?})",
            driver_status(&hub, "teleinfo")
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The first open attempt succeeded, so the driver sits in its read loop,
    // not waiting for a port or in a retry backoff.
    assert!(!matches!(
        driver_status(&hub, "teleinfo"),
        DriverStatus::Waiting { .. } | DriverStatus::Backoff { .. }
    ));

    let feeder = feed(master, data);

    // Wait until the whole capture is applied. EAST reaches its final
    // 21849109 Wh in the second-to-last frame, so the last frame's SINSTS
    // (02698 -> 269.8 VA, held by no earlier frame) marks its arrival.
    let deadline = Instant::now() + Duration::from_secs(15);
    while value_of(&hub, "teleinfo:linky/sinsts") != Some(Value::Float(269.8)) {
        assert!(
            Instant::now() < deadline,
            "sinsts never reached 269.8 (last value {:?})",
            value_of(&hub, "teleinfo:linky/sinsts")
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Final values, straight from the capture's last frame (text padding
    // trimmed; numeric points published as Float, sub-units divided out:
    // 003 -> 0.3 A, 237 -> 23.7 V, 02698 -> 269.8 VA, EAST in Wh).
    assert_eq!(
        value_of(&hub, "teleinfo:linky/adsc"),
        Some(Value::Text("000000000002".into()))
    );
    assert_eq!(
        value_of(&hub, "teleinfo:linky/ngtf"),
        Some(Value::Text("TEMPO".into()))
    );
    assert_eq!(
        value_of(&hub, "teleinfo:linky/ltarf"),
        Some(Value::Text("HC  BLANC".into()))
    );
    assert_eq!(
        value_of(&hub, "teleinfo:linky/east"),
        Some(Value::Float(21_849_109.0))
    );
    assert_eq!(
        value_of(&hub, "teleinfo:linky/irms1"),
        Some(Value::Float(0.3))
    );
    assert_eq!(
        value_of(&hub, "teleinfo:linky/urms1"),
        Some(Value::Float(23.7))
    );
    assert_eq!(
        value_of(&hub, "teleinfo:linky/sinsts"),
        Some(Value::Float(269.8))
    );

    // The meter counts online as soon as a valid frame arrives.
    let device = hub
        .device(&DeviceId::from("teleinfo:linky"))
        .expect("device registered");
    assert_eq!(device.online, Some(true));

    // Teardown: let the feed finish first — closing the master then hits the
    // driver's reader with an EOF, and the cancel stops the driver.
    feeder
        .join()
        .expect("feeder thread")
        .expect("feed completed");
    cancel.cancel();
    handle.await.expect("supervisor must stop on cancel");
}

#[tokio::test]
async fn a_missing_port_holds_the_driver_waiting() {
    let hub = Hub::new(HubOptions::default()).expect("hub");
    let cancel = CancellationToken::new();
    let driver = Arc::new(
        Teleinfo::new(config_with_port(
            "/dev/moli-teleinfo-test-does-not-exist".into(),
        ))
        .expect("config"),
    );
    let handle = spawn_driver(&hub, InstanceId::from("teleinfo"), driver, cancel.clone());

    // The first open attempt fails almost immediately; the driver then holds
    // `waiting` and backs off until the port appears.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = driver_status(&hub, "teleinfo");
        if matches!(status, DriverStatus::Waiting { .. }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "driver never entered waiting (status {status:?})"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The device is known from the start, but offline: the meter was never seen.
    let device = hub
        .device(&DeviceId::from("teleinfo:linky"))
        .expect("device registered");
    assert_eq!(device.online, Some(false));

    cancel.cancel();
    handle.await.expect("supervisor must stop on cancel");
}

#[tokio::test]
async fn the_driver_reconnects_when_the_port_appears() {
    let (mut master, slave_path) = open_pty();
    let data = standard_capture();

    // A stable path (think /dev/ttyTIC) that does not exist when the driver
    // starts, then appears: the module is plugged in after boot.
    let tmp = std::env::temp_dir().join(format!("moli-teleinfo-{}", std::process::id()));
    fs::create_dir_all(&tmp).expect("temp dir");
    let port_path = tmp.join("ttyTIC");
    let _ = fs::remove_file(&port_path); // a previous failed run may have left it
    let port = port_path.to_string_lossy().into_owned();

    let hub = Hub::new(HubOptions::default()).expect("hub");
    let cancel = CancellationToken::new();
    let driver = Arc::new(Teleinfo::new(config_with_port(port)).expect("config"));
    let handle = spawn_driver(&hub, InstanceId::from("teleinfo"), driver, cancel.clone());

    // The plug-in event: the symlink appears, then the meter starts pushing.
    let feeder = thread::spawn(move || -> std::io::Result<()> {
        thread::sleep(Duration::from_millis(500));
        std::os::unix::fs::symlink(&slave_path, &port_path)?;
        thread::sleep(Duration::from_millis(200));
        for chunk in data.chunks(512) {
            master.write_all(chunk)?;
            thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    });

    // The driver backs off (1 s, 2 s, 4 s…) until the port exists, then
    // reconnects and applies the whole capture — the last frame's SINSTS
    // (269.8 VA) is the marker that the whole stream was parsed.
    let deadline = Instant::now() + Duration::from_secs(30);
    while value_of(&hub, "teleinfo:linky/sinsts") != Some(Value::Float(269.8)) {
        assert!(
            Instant::now() < deadline,
            "sinsts never reached 269.8 (status {:?}, last value {:?})",
            driver_status(&hub, "teleinfo"),
            value_of(&hub, "teleinfo:linky/sinsts")
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let device = hub
        .device(&DeviceId::from("teleinfo:linky"))
        .expect("device registered");
    assert_eq!(device.online, Some(true));

    feeder
        .join()
        .expect("feeder thread")
        .expect("feed completed");
    cancel.cancel();
    handle.await.expect("supervisor must stop on cancel");
    let _ = fs::remove_dir_all(&tmp);
}
