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
fn error_kinds(mut debug: &str) -> String {
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

pub(super) async fn pair(
    controller: &MatterController,
    ctx: &DriverCtx,
    ble_thread: bool,
) -> anyhow::Result<()> {
    let Some(code) = ctx.secret("pairing_code").filter(|s| !s.trim().is_empty()) else {
        return Ok(());
    };
    anyhow::ensure!(code.len() <= 512, invalid());
    // Consume the operator's one-shot request BEFORE any network action. A
    // partial commission followed by supervisor restart must never retry by
    // itself and fill the device's fabric table or repeat an uncertain action.
    if ble_thread {
        #[cfg(not(feature = "ble"))]
        anyhow::bail!(moli_i18n::tr!("pilotes.matter.ble_unavailable"));
        #[cfg(feature = "ble")]
        {
            let dataset = dataset(&ctx.secret("thread_dataset").ok_or_else(invalid)?)?;
            ctx.forget_secret("pairing_code")
                .await
                .map_err(|_| invalid())?;
            controller
                .commission_ble(
                    code.trim(),
                    matter_controller::NetworkCredentials::Thread(dataset),
                    None,
                )
                .await
                .map_err(|error| failed(&error))?;
            return Ok(());
        }
    }
    ctx.forget_secret("pairing_code")
        .await
        .map_err(|_| invalid())?;
    controller
        .commission(code.trim(), None)
        .await
        .map_err(|error| failed(&error))?;
    Ok(())
}

#[cfg(any(feature = "ble", test))]
fn dataset(hex: &str) -> anyhow::Result<matter_controller::ThreadDataset> {
    let hex = hex.trim();
    anyhow::ensure!(hex.len() <= 508 && hex.len().is_multiple_of(2), invalid());
    let bytes = hex
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |b: u8| match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                b'A'..=b'F' => Some(b - b'A' + 10),
                _ => None,
            };
            Ok(digit(pair[0]).ok_or_else(invalid)? * 16 + digit(pair[1]).ok_or_else(invalid)?)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    matter_controller::ThreadDataset::new(bytes).map_err(|_| invalid())
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

    #[test]
    fn thread_input_is_bounded_and_errors_never_echo_credentials() {
        // Synthetic structural TLV: Extended PAN ID, not a real active dataset.
        assert!(dataset("02080000000000000001").is_ok());
        for value in ["", "secret-key-value", "éé", "020800", "0"] {
            let error = dataset(value).unwrap_err().to_string();
            if !value.is_empty() {
                assert!(!error.contains(value));
            }
        }
        assert!(dataset(&"00".repeat(255)).is_err());
    }
}
