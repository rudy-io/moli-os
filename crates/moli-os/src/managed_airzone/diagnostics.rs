//! Bounded manual reads, independent of polling and never imported as state.
use super::{Manager, SetupError};
use moli_api::airzone_setup::{GatewayDiagnosticReport, GatewayDiagnosticRequest};

impl Manager {
    pub(super) async fn inspect_gateway(
        &self,
        instance: String,
        request: GatewayDiagnosticRequest,
    ) -> Result<GatewayDiagnosticReport, SetupError> {
        self.available()?;
        let _permit = self
            .0
            .diagnostics
            .try_acquire()
            .map_err(|_| SetupError::new("busy"))?;
        let config = {
            let state = self
                .0
                .state
                .try_lock()
                .map_err(|_| SetupError::new("busy"))?;
            let gateway = self.existing(&state, &instance)?;
            let mut config = gateway.config.clone();
            // Preserve a learned identity even when TOML predates discovery.
            config.mac = self.known_mac(&gateway);
            config.read_only = true;
            config
        };
        let request = native_request(&request);
        request
            .validate()
            .map_err(|_| SetupError::new("invalid_request"))?;
        let report = tokio::select! {
            () = self.0.cancel.cancelled() => return Err(SetupError::new("shutting_down")),
            report = moli_airzone::inspect(config, request) => report.map_err(|_| SetupError::new("probe_failed"))?,
        };
        serde_json::from_value(report).map_err(|_| SetupError::new("probe_failed"))
    }
}

fn native_request(request: &GatewayDiagnosticRequest) -> moli_airzone::DiagnosticRequest {
    use moli_airzone::DiagnosticRequest as Native;
    match request {
        GatewayDiagnosticRequest::Hvac { system, zone, get } => Native::Hvac {
            system: *system,
            zone: *zone,
            get: *get,
        },
        GatewayDiagnosticRequest::Iaq {
            system,
            sensor,
            get,
        } => Native::Iaq {
            system: *system,
            sensor: *sensor,
            get: *get,
        },
        GatewayDiagnosticRequest::Version {} => Native::Version {},
        GatewayDiagnosticRequest::Webserver {} => Native::Webserver {},
        GatewayDiagnosticRequest::Integration {} => Native::Integration {},
        GatewayDiagnosticRequest::Demo {} => Native::Demo {},
        GatewayDiagnosticRequest::Thermostat {} => Native::Thermostat {},
    }
}
