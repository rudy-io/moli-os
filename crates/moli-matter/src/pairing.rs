use matter_controller::MatterController;
use moli_runtime::DriverCtx;

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
                .map_err(|_| invalid())?;
            return Ok(());
        }
    }
    ctx.forget_secret("pairing_code")
        .await
        .map_err(|_| invalid())?;
    controller
        .commission(code.trim(), None)
        .await
        .map_err(|_| invalid())?;
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
