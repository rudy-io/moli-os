//! What an exchange costs in the provider's credit, estimated from the usage
//! it reports and public prices (dollars, October 2026). Prices change: an
//! estimate for the dashboard and the recap, never an invoice.

/// Dollars per million tokens: input, input read from the provider's cache,
/// output.
struct Rates {
    input: f64,
    cached: f64,
    output: f64,
}

/// A thinking model's rates (the longest matching name first).
fn rates(model: &str) -> Option<Rates> {
    let (input, cached, output) = match model {
        m if m.starts_with("gpt-4.1-nano") => (0.10, 0.025, 0.40),
        m if m.starts_with("gpt-4.1-mini") => (0.40, 0.10, 1.60),
        m if m.starts_with("gpt-4.1") => (2.00, 0.50, 8.00),
        m if m.starts_with("gpt-4o-mini") => (0.15, 0.075, 0.60),
        m if m.starts_with("gpt-4o") => (2.50, 1.25, 10.00),
        m if m.starts_with("gpt-5-nano") => (0.05, 0.005, 0.40),
        m if m.starts_with("gpt-5-mini") => (0.25, 0.025, 2.00),
        m if m.starts_with("gpt-5") => (1.25, 0.125, 10.00),
        _ => return None,
    };
    Some(Rates {
        input,
        cached,
        output,
    })
}

/// Thinking: `prompt` tokens (of which `cached` came from the cache) and
/// `completion` tokens. 0 for a model without known prices (a local one).
#[allow(clippy::cast_precision_loss)]
pub(crate) fn think(model: &str, prompt: u64, cached: u64, completion: u64) -> f64 {
    rates(model).map_or(0.0, |r| {
        let fresh = prompt.saturating_sub(cached) as f64;
        (fresh * r.input + cached.min(prompt) as f64 * r.cached + completion as f64 * r.output)
            / 1_000_000.0
    })
}

/// Speech to text, by the minute.
pub(crate) fn hear(model: &str, seconds: f64) -> f64 {
    let per_minute = match model {
        "" => 0.0,
        m if m.starts_with("gpt-4o-mini-transcribe") => 0.003,
        _ => 0.006,
    };
    per_minute * seconds / 60.0
}

/// Text to speech: about 15 characters a second at the house's pace.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn speak(model: &str, chars: usize) -> f64 {
    let per_minute = match model {
        "" => 0.0,
        m if m.starts_with("gpt-4o-mini-tts") => 0.015,
        _ => 0.03,
    };
    per_minute * chars as f64 / 15.0 / 60.0
}

/// One web search (the provider bills each call, then its tokens).
pub(crate) const SEARCH: f64 = 0.025;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_costs_its_tokens() {
        // 6 000 prompt tokens, 5 000 of them from the cache, 50 out.
        let cost = think("gpt-4.1-mini", 6_000, 5_000, 50);
        let expected = (1_000.0 * 0.40 + 5_000.0 * 0.10 + 50.0 * 1.60) / 1_000_000.0;
        assert!((cost - expected).abs() < 1e-12, "{cost}");
        assert!(think("gpt-4.1-mini-2025-04-14", 1_000, 0, 0) > 0.0);
        assert!(think("llama3", 1_000, 0, 0).abs() < f64::EPSILON);
        assert!((hear("gpt-4o-mini-transcribe", 60.0) - 0.003).abs() < 1e-12);
        assert!((speak("gpt-4o-mini-tts", 900) - 0.015).abs() < 1e-12);
        assert!(speak("", 900).abs() < f64::EPSILON);
    }
}
