//! Local/nightly integration harness driving connectedhomeip's all-clusters-app.
//! Tests early-return ("skipped") unless `MATTER_INTEGRATION_DUT` is set (by
//! `xtask integration`), so the normal `cargo test` / gate compiles + skips them.
use std::path::PathBuf;

pub mod events;
pub mod fixture;
pub mod sweep;

/// Device-under-test configuration and helpers.
pub mod dut {
    use super::PathBuf;

    /// Configuration for a live device-under-test, sourced from env by
    /// `xtask integration`. `None` => no DUT available => tests skip.
    #[derive(Clone, Debug)]
    pub struct DutConfig {
        /// QR/manual setup code to commission the DUT.
        pub setup_code: String,
        /// connectedhomeip checkout root (for dev-cert attestation dirs).
        pub chip_root: PathBuf,
        /// Multicast egress interface index for group-cast (`MATTER_MULTICAST_IF`).
        pub multicast_if: Option<u32>,
        /// Absolute dir for per-run DUT state (controller store + node-id sidecar),
        /// supplied by `xtask integration` so paths don't depend on cargo's test cwd.
        pub dut_dir: PathBuf,
        /// Which connectedhomeip example app is the running DUT
        /// (`MATTER_INTEGRATION_DUT_APP`, default `all-clusters`). App-specific
        /// tests gate on this via [`DutConfig::is_app`].
        pub app: String,
        /// The DUT's out-of-band command FIFO (`--app-pipe`), when the harness
        /// launched it with one (`MATTER_INTEGRATION_APP_PIPE`): all-clusters
        /// and lock-app. See [`crate::events::send_app_pipe`].
        pub app_pipe: Option<PathBuf>,
    }

    impl DutConfig {
        /// Read DUT config from env; `None` if `MATTER_INTEGRATION_DUT` is unset.
        #[must_use]
        pub fn from_env() -> Option<DutConfig> {
            let setup_code = std::env::var("MATTER_INTEGRATION_DUT").ok()?;
            let chip_root = std::env::var("CHIP_ROOT")
                .unwrap_or_else(|_| "/Users/hemanshubhojak/code/connectedhomeip".into())
                .into();
            let multicast_if = std::env::var("MATTER_MULTICAST_IF")
                .ok()
                .and_then(|s| s.parse().ok());
            let dut_dir = std::env::var("MATTER_INTEGRATION_DUT_DIR")
                .map_or_else(|_| PathBuf::from("target/integration-dut"), PathBuf::from);
            let app = std::env::var("MATTER_INTEGRATION_DUT_APP")
                .unwrap_or_else(|_| "all-clusters".to_string());
            let app_pipe = std::env::var_os("MATTER_INTEGRATION_APP_PIPE").map(PathBuf::from);
            Some(DutConfig {
                setup_code,
                chip_root,
                multicast_if,
                dut_dir,
                app,
                app_pipe,
            })
        }

        /// True when the named connectedhomeip example app is the running DUT
        /// (set by `xtask integration <app>`). The default DUT is `all-clusters`.
        #[must_use]
        pub fn is_app(&self, name: &str) -> bool {
            self.app == name
        }

        /// Per-run controller store path.
        #[must_use]
        pub fn store_path(&self) -> PathBuf {
            self.dut_dir.join("controller-store.bin")
        }

        /// Per-run node-id sidecar path (records the commissioned node id so later
        /// tests in the same run reconnect instead of re-commissioning).
        #[must_use]
        pub fn node_sidecar(&self) -> PathBuf {
            self.dut_dir.join("node-id.txt")
        }

        /// Development PAA root certs dir.
        #[must_use]
        pub fn paa_dir(&self) -> PathBuf {
            self.chip_root
                .join("credentials/development/paa-root-certs")
        }

        /// Development CD signing certs dir.
        #[must_use]
        pub fn cd_dir(&self) -> PathBuf {
            self.chip_root.join("credentials/development/cd-certs")
        }
    }
}

/// Skip-guard: returns from a `#[tokio::test]` early (logging) when no DUT.
#[macro_export]
macro_rules! dut_or_skip {
    () => {{
        match $crate::dut::DutConfig::from_env() {
            Some(c) => c,
            None => {
                eprintln!("skipped: no DUT (set MATTER_INTEGRATION_DUT via `just integration`)");
                return;
            }
        }
    }};
}

/// Compile-checks the Rust examples in the workspace root `README.md`.
///
/// The root README lives outside any crate directory, so `include_str!` on it
/// would break `cargo package` verification for a published crate. This
/// harness crate is `publish = false` and already depends on
/// `matter-controller` + Tokio, which is exactly what the root README's
/// quickstart needs, so it hosts the check instead.
#[cfg(doctest)]
#[doc = include_str!("../../../README.md")]
struct RootReadmeDoctests;
