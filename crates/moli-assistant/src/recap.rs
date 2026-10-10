//! The evening recap: the day's exchanges with Moli, in a message the
//! house's Telegram sends. Plain counts first, then each exchange, briefly.

use jiff::Zoned;
use jiff::civil::Time;
use jiff::tz::TimeZone;

use crate::exchanges::{Exchange, Kind};

/// At most this many exchanges listed (a Telegram message holds 4096 characters).
const LISTED: usize = 30;
const QUESTION_CHARS: usize = 80;
const REPLY_CHARS: usize = 100;

/// `21:00` → 21 h; `None` for anything else (no recap).
pub(crate) fn parse_time(text: &str) -> Option<Time> {
    let (hour, minute) = text.trim().split_once(':')?;
    Time::new(hour.parse().ok()?, minute.parse().ok()?, 0, 0).ok()
}

/// The next time `at` comes, strictly after `now`.
pub(crate) fn next_after(now: &Zoned, at: Time) -> Option<Zoned> {
    let today = now.with().time(at).build().ok()?;
    if today > *now {
        Some(today)
    } else {
        today.checked_add(jiff::Span::new().days(1)).ok()
    }
}

/// The message; `None` when nothing happened (no message then).
pub(crate) fn compose(exchanges: &[Exchange], tz: &TimeZone, day: &str) -> Option<String> {
    let turns: Vec<&Exchange> = exchanges
        .iter()
        .filter(|e| e.kind != Kind::Silence)
        .collect();
    let silences = exchanges.len() - turns.len();
    if exchanges.is_empty() {
        return None;
    }
    let satellite = turns.iter().filter(|e| e.surface == "satellite").count();
    let orders: usize = turns.iter().map(|e| e.orders.len()).sum();
    let searches = turns
        .iter()
        .filter(|e| e.tools.iter().any(|t| t == "web_search"))
        .count();
    let failed = turns.iter().filter(|e| e.kind == Kind::Failed).count();
    let mut lines = vec![
        moli_i18n::tr!("assistant.recap.title", day = day),
        moli_i18n::tr!(
            "assistant.recap.counts",
            turns = turns.len(),
            satellite = satellite,
            app = turns.len() - satellite,
            orders = orders,
            searches = searches
        ),
    ];
    if silences > 0 {
        lines.push(moli_i18n::tr!("assistant.recap.silences", n = silences));
    }
    if failed > 0 {
        lines.push(moli_i18n::tr!("assistant.recap.failed", n = failed));
    }
    let cost: f64 = exchanges.iter().map(|e| e.cost.total).sum();
    if cost > 0.0 {
        lines.push(moli_i18n::tr!(
            "assistant.recap.cost",
            dollars = money(cost)
        ));
    }
    let answered: Vec<u64> = turns
        .iter()
        .filter(|e| e.kind == Kind::Turn)
        .map(|e| e.ms)
        .collect();
    if !answered.is_empty() {
        #[allow(clippy::cast_precision_loss)]
        let mean = answered.iter().sum::<u64>() as f64 / answered.len() as f64 / 1000.0;
        lines.push(moli_i18n::tr!("assistant.recap.speed", s = decimal(mean)));
    }
    if !turns.is_empty() {
        lines.push(String::new());
    }
    let skipped = turns.len().saturating_sub(LISTED);
    for e in &turns[skipped..] {
        let time = jiff::Timestamp::from_millisecond(e.at)
            .map(|t| t.to_zoned(tz.clone()).strftime("%H:%M").to_string())
            .unwrap_or_default();
        let place = if e.surface == "satellite" {
            moli_i18n::tr!("assistant.recap.boitier")
        } else {
            moli_i18n::tr!("assistant.recap.appli")
        };
        let reply = if e.kind == Kind::Failed {
            moli_i18n::tr!("assistant.recap.echec")
        } else if e.reply.trim().is_empty() {
            moli_i18n::tr!("assistant.recap.fin")
        } else {
            short(&e.reply, REPLY_CHARS)
        };
        lines.push(moli_i18n::tr!(
            "assistant.recap.line",
            time = time,
            place = place,
            question = short(&e.question, QUESTION_CHARS),
            reply = reply
        ));
    }
    if skipped > 0 {
        lines.push(moli_i18n::tr!("assistant.recap.more", n = skipped));
    }
    Some(lines.join("\n"))
}

/// Dollars to the cent, below a cent to the tenth of a cent.
pub(crate) fn money(dollars: f64) -> String {
    let text = if dollars < 0.01 {
        format!("{dollars:.3}")
    } else {
        format!("{dollars:.2}")
    };
    if moli_i18n::language().starts_with("fr") {
        text.replace('.', ",")
    } else {
        text
    }
}

/// `1.36` → `1,4` in French, `1.4` otherwise.
fn decimal(value: f64) -> String {
    let text = format!("{value:.1}");
    if moli_i18n::language().starts_with("fr") {
        text.replace('.', ",")
    } else {
        text
    }
}

/// At most `max` characters, an ellipsis when cut, on one line.
fn short(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchanges::Order;

    fn exchange(at: i64, kind: Kind, surface: &str, question: &str, reply: &str) -> Exchange {
        Exchange {
            at,
            kind,
            surface: surface.into(),
            spoken: true,
            question: question.into(),
            reply: reply.into(),
            tools: Vec::new(),
            orders: Vec::new(),
            ms: 1_000,
            audio: None,
            cost: crate::exchanges::Cost::new(0.001, 0.0, 0.0, 0.0),
        }
    }

    #[test]
    fn the_recap_counts_then_lists() {
        let tz = TimeZone::get("Europe/Paris").unwrap();
        // 2026-10-10 15:20 in Paris.
        let at = 1_791_552_000_000 + 4 * 3600 * 1000;
        let mut order = exchange(
            at,
            Kind::Turn,
            "satellite",
            "Allume le salon",
            "C'est allumé.",
        );
        order.orders.push(Order {
            device: "Plafonnier".into(),
            status: "done".into(),
        });
        let mut search = exchange(
            at + 60_000,
            Kind::Turn,
            "bubble",
            "Les nouvelles du jour ?",
            "Il pleut.",
        );
        search.tools.push("web_search".into());
        let all = [
            order,
            search,
            exchange(at + 120_000, Kind::Silence, "satellite", "", ""),
            exchange(
                at + 180_000,
                Kind::Failed,
                "satellite",
                "Quelle heure ?",
                "busy",
            ),
        ];
        let text = compose(&all, &tz, "samedi 10 octobre").unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("samedi 10 octobre"), "{text}");
        assert!(lines[1].contains('3'), "{text}");
        // Three exchanges listed (not the silence), in order.
        let listed: Vec<&&str> = lines.iter().filter(|l| l.contains("→")).collect();
        assert_eq!(listed.len(), 3, "{text}");
        assert!(listed[0].contains("Allume le salon"), "{text}");
        assert_eq!(compose(&[], &tz, "samedi"), None);
    }

    #[test]
    fn the_next_recap_is_tonight_or_tomorrow() {
        let tz = TimeZone::get("Europe/Paris").unwrap();
        let at = parse_time("21:00").unwrap();
        let afternoon: Zoned = "2026-10-10T15:00:00+02:00[Europe/Paris]".parse().unwrap();
        let night: Zoned = "2026-10-10T22:00:00+02:00[Europe/Paris]".parse().unwrap();
        assert_eq!(next_after(&afternoon, at).unwrap().hour(), 21);
        assert_eq!(next_after(&afternoon, at).unwrap().day(), 10);
        assert_eq!(next_after(&night, at).unwrap().day(), 11);
        assert_eq!(parse_time(""), None);
        assert_eq!(parse_time("25:00"), None);
        let _ = tz;
    }

    #[test]
    fn long_words_are_cut() {
        assert_eq!(short("un  deux\ntrois", 20), "un deux trois");
        assert_eq!(short("abcdefghij", 5), "abcd…");
    }
}
