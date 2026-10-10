//! What the house does not know: the weather to come (Open-Meteo, free and
//! keyless, the service the weather driver already reads) and the web (the
//! provider's own search: `/responses` with its `web_search` tool).

use std::time::Duration;

use http::{Method, Request};
use http_body_util::Full;
use moli_net::Body as Bytes;
use serde_json::{Value as Json, json};

use crate::llm::Endpoint;

const FORECAST_HOST: &str = "api.open-meteo.com";
const FORECAST_LIMIT: Duration = Duration::from_secs(10);
const MAX_FORECAST: usize = 256 * 1024;
/// The next hours, one every three: enough for « cet après-midi ».
const HOURS_STEP: usize = 3;

/// The days to come (1 to 7, today first) and the next 24 hours.
pub(crate) async fn forecast(latitude: f64, longitude: f64, days: u64) -> anyhow::Result<Json> {
    let days = days.clamp(1, 7);
    let path = format!(
        "/v1/forecast?latitude={latitude:.3}&longitude={longitude:.3}\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,\
         precipitation_probability_max,wind_speed_10m_max\
         &hourly=temperature_2m,precipitation_probability,weather_code\
         &forecast_hours=24&forecast_days={days}&timezone=auto"
    );
    let request = Request::builder()
        .method(Method::GET)
        .uri(path)
        .header("accept", "application/json")
        .body(Full::new(Bytes::new()))?;
    let (status, body) = moli_net::web(
        FORECAST_HOST,
        443,
        true,
        request,
        FORECAST_LIMIT,
        MAX_FORECAST,
    )
    .await?;
    anyhow::ensure!(status.is_success(), "open-meteo: HTTP {}", status.as_u16());
    Ok(summary(&serde_json::from_slice(&body)?))
}

/// Open-Meteo's columns as rows the model reads at a glance.
fn summary(w: &Json) -> Json {
    let d = &w["daily"];
    let days: Vec<Json> = d["time"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, date)| {
            json!({
                "date": date,
                "sky": sky(d["weather_code"][i].as_u64()),
                "min_c": d["temperature_2m_min"][i],
                "max_c": d["temperature_2m_max"][i],
                "rain_mm": d["precipitation_sum"][i],
                "rain_chance_pct": d["precipitation_probability_max"][i],
                "wind_max_kmh": d["wind_speed_10m_max"][i],
            })
        })
        .collect();
    let h = &w["hourly"];
    let hours: Vec<Json> = h["time"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .step_by(HOURS_STEP)
        .map(|(i, at)| {
            json!({
                "at": at,
                "sky": sky(h["weather_code"][i].as_u64()),
                "temperature_c": h["temperature_2m"][i],
                "rain_chance_pct": h["precipitation_probability"][i],
            })
        })
        .collect();
    json!({ "days": days, "next_hours": hours })
}

/// A WMO weather code in the house's words.
fn sky(code: Option<u64>) -> String {
    let key = match code {
        Some(0) => "assistant.weather.clear",
        Some(1 | 2) => "assistant.weather.partly",
        Some(3) => "assistant.weather.cloudy",
        Some(45 | 48) => "assistant.weather.fog",
        Some(51..=57) => "assistant.weather.drizzle",
        Some(61..=67) => "assistant.weather.rain",
        Some(71..=77 | 85 | 86) => "assistant.weather.snow",
        Some(80..=82) => "assistant.weather.showers",
        Some(95..=99) => "assistant.weather.storm",
        _ => return String::new(),
    };
    moli_i18n::tr(key)
}

/// A question answered from the web, in a few sentences.
pub(crate) async fn search(
    endpoint: &Endpoint,
    key: Option<&str>,
    model: &str,
    timezone: &str,
    query: &str,
) -> anyhow::Result<Json> {
    let body = request(model, timezone, query);
    let answer = endpoint.respond(key, &body).await?;
    let text = output_text(&answer);
    anyhow::ensure!(!text.is_empty(), "the search gave no answer");
    Ok(json!({ "answer": text }))
}

fn request(model: &str, timezone: &str, query: &str) -> Json {
    json!({
        "model": model,
        "tools": [{
            "type": "web_search",
            "search_context_size": "low",
            "user_location": { "type": "approximate", "timezone": timezone },
        }],
        "input": format!("{query}\n\n{}", moli_i18n::tr("assistant.search.style")),
        "max_output_tokens": 500,
    })
}

/// The text of a `/responses` answer: its messages' `output_text` parts.
fn output_text(answer: &Json) -> String {
    answer["output"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "message")
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter(|part| part["type"] == "output_text")
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_forecast_reads_as_rows() {
        let w = json!({
            "daily": {
                "time": ["2026-10-10", "2026-10-11"],
                "weather_code": [61, 0],
                "temperature_2m_max": [19.4, 22.1],
                "temperature_2m_min": [12.0, 11.5],
                "precipitation_sum": [4.2, 0.0],
                "precipitation_probability_max": [80, 5],
                "wind_speed_10m_max": [25.0, 12.0]
            },
            "hourly": {
                "time": ["2026-10-10T16:00", "2026-10-10T17:00", "2026-10-10T18:00", "2026-10-10T19:00"],
                "temperature_2m": [18.0, 17.5, 17.0, 16.0],
                "precipitation_probability": [70, 60, 50, 40],
                "weather_code": [61, 61, 3, 3]
            }
        });
        let s = summary(&w);
        assert_eq!(s["days"].as_array().unwrap().len(), 2);
        assert_eq!(s["days"][1]["max_c"], 22.1);
        assert_eq!(s["days"][0]["rain_chance_pct"], 80);
        // One hour in three.
        let hours = s["next_hours"].as_array().unwrap();
        assert_eq!(hours.len(), 2);
        assert_eq!(hours[1]["at"], "2026-10-10T19:00");
        assert!(!s["days"][0]["sky"].as_str().unwrap().is_empty());
        assert_eq!(sky(None), "");
        assert_eq!(sky(Some(200)), "");
    }

    #[test]
    fn a_search_answer_is_its_text() {
        let answer = json!({
            "output": [
                { "type": "web_search_call", "status": "completed" },
                { "type": "message", "content": [
                    { "type": "output_text", "text": "Le match a lieu à 21 h.", "annotations": [] }
                ]}
            ]
        });
        assert_eq!(output_text(&answer), "Le match a lieu à 21 h.");
        assert_eq!(output_text(&json!({ "output": [] })), "");
    }

    #[test]
    fn a_search_asks_for_the_web_tool_where_the_house_is() {
        let body = request(
            "gpt-4.1-mini",
            "Europe/Paris",
            "Quand joue l'équipe de France ?",
        );
        assert_eq!(body["tools"][0]["type"], "web_search");
        assert_eq!(
            body["tools"][0]["user_location"]["timezone"],
            "Europe/Paris"
        );
        assert!(
            body["input"]
                .as_str()
                .unwrap()
                .starts_with("Quand joue l'équipe de France ?")
        );
    }
}
