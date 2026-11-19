use matter_controller::MatterController;
use moli_runtime::DriverCtx;

// Log only a bounded category: upstream errors can contain network identifiers.
fn failed(error: &matter_controller::Error) -> anyhow::Error {
    let kinds = error_kinds(&format!("{error:?}"));
    let category = match error {
        matter_controller::Error::SetupCode(_) => "setup_code",
        matter_controller::Error::Driver(_) => "commissioning_driver",
        matter_controller::Error::Transport(_) => "transport",
        matter_controller::Error::Trust(_) | matter_controller::Error::NoTrust => {
            "attestation_trust"
        }
        matter_controller::Error::Store(_) => "persistence",
        _ => "controller",
    };
    tracing::warn!(category, kinds, "Matter commissioning failed");
    invalid()
}

// Keep only leading enum variant names, never fields, quoted strings or numbers.
// The pinned stack's nested Debug enums expose the failing protocol phase this way
// without adding a dependency on its internal error types.
pub(super) fn error_kinds(mut debug: &str) -> String {
    let mut kinds = Vec::new();
    for _ in 0..6 {
        let end = debug
            .find(|c: char| !c.is_ascii_alphabetic() && c != '_')
            .unwrap_or(debug.len());
        let name = &debug[..end];
        if !name.starts_with(char::is_uppercase) {
            break;
        }
        kinds.push(name);
        let Some(inner) = debug[end..].strip_prefix('(') else {
            break;
        };
        debug = inner;
    }
    kinds.join("/")
}

fn invalid() -> anyhow::Error {
    anyhow::anyhow!(moli_i18n::tr!("pilotes.matter.pairing_failed"))
}

/// Pairs the device a human opened a window for: found by mDNS, or at the
/// `address` the configuration gives (with its sleep pace, if it sleeps).
pub(super) async fn pair(
    controller: &MatterController,
    ctx: &DriverCtx,
    address: Option<std::net::SocketAddr>,
    pace: Option<(std::time::Duration, std::time::Duration)>,
) -> anyhow::Result<()> {
    let Some(code) = ctx.secret("pairing_code").filter(|s| !s.trim().is_empty()) else {
        return Ok(());
    };
    // Consume the operator's one-shot request BEFORE any network action. A
    // partial commission followed by supervisor restart must never retry by
    // itself and fill the device's fabric table or repeat an uncertain action.
    ctx.forget_secret("pairing_code")
        .await
        .map_err(|_| invalid())?;
    anyhow::ensure!(code.len() <= 512, invalid());
    let result = match address {
        Some(address) => {
            controller
                .commission_at(code.trim(), address, pace, None)
                .await
        }
        None => controller.commission(code.trim(), None).await,
    };
    result.map_err(|error| failed(&error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_never_includes_error_fields() {
        assert_eq!(
            error_kinds("Driver(Commissioning(Attestation(MissingPaa)))"),
            "Driver/Commissioning/Attestation/MissingPaa"
        );
        for value in [
            "SetupCode(\"private-code\")",
            "Driver(Discovery(\"private-host\"))",
            "Driver(Timeout { exchange_id: 12345 })",
        ] {
            let kinds = error_kinds(value);
            assert!(!kinds.contains("private"));
            assert!(!kinds.chars().any(|c| c.is_ascii_digit()));
        }
    }
}
