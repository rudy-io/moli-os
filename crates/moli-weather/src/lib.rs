//! The sky over the house, for the dashboard (Open-Meteo, free and keyless,
//! the service the weather driver already reads):
//!
//! - the forecast: the next hours and the days to come;
//! - the weather map: a grid of points around the house, hour by hour for
//!   the next day (wind, clouds, rain), and the land under it (elevations),
//!   from which the dashboard draws its own map: relief, clouds, rain and
//!   the wind flowing.
//!
//! Nothing is fetched until someone looks, then it is kept a while: a wall
//! of tablets costs the service one request. The land never changes: asked
//! once, kept on disk. The map is asked around a point rounded to a tenth
//! of a degree.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{StreamExt, TryStreamExt};
use http::{Method, Request};
use http_body_util::Full;
use moli_net::Body as Bytes;
use serde_json::{Value as Json, json};
use tokio::sync::Mutex;

const HOST: &str = "api.open-meteo.com";
/// Open-Meteo refreshes its models every hour or so.
const FORECAST_KEEP: Duration = Duration::from_secs(15 * 60);
const MAP_KEEP: Duration = Duration::from_secs(60 * 60);
const LIMIT: Duration = Duration::from_secs(15);
const MAX_FORECAST: usize = 256 * 1024;
const MAX_MAP: usize = 2 * 1024 * 1024;
const MAX_ELEVATION: usize = 64 * 1024;

/// The map: 400 × 300 km around the house.
pub const MAP_WIDTH_KM: f64 = 400.0;
pub const MAP_HEIGHT_KM: f64 = 300.0;
/// The weather's grid (one request; ~35 km between points, about the
/// models' own mesh): the dashboard smooths between them.
pub const WEATHER_GRID: [usize; 2] = [12, 9];
/// The land's grid (~6 km between points), asked a hundred at a time.
pub const LAND_GRID: [usize; 2] = [64, 48];
const ELEVATIONS_PER_CALL: usize = 100;
/// The next day, hour by hour.
const MAP_HOURS: u32 = 24;
const KM_PER_DEGREE: f64 = 111.32;

#[derive(Clone, Debug)]
pub struct Weather(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    latitude: f64,
    longitude: f64,
    /// Where the land is kept (`weather-land.json`), when there is a disk.
    data_dir: Option<PathBuf>,
    forecast: Mutex<Option<(Instant, Json)>>,
    map: Mutex<Option<(Instant, Json)>>,
    land: Mutex<Option<Arc<Vec<f32>>>>,
}

/// The map's area (degrees) and where the house sits on it (0..1 from the
/// left, from the top).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
    pub house: [f64; 2],
}

impl Area {
    #[must_use]
    pub fn around(latitude: f64, longitude: f64) -> Self {
        // Asked around a rounded point (~10 km): the service never learns
        // where the house is.
        let c_lat = (latitude * 10.0).round() / 10.0;
        let c_lon = (longitude * 10.0).round() / 10.0;
        let half_lat = MAP_HEIGHT_KM / 2.0 / KM_PER_DEGREE;
        let half_lon = MAP_WIDTH_KM / 2.0 / (KM_PER_DEGREE * c_lat.to_radians().cos());
        let (south, north) = (c_lat - half_lat, c_lat + half_lat);
        let (west, east) = (c_lon - half_lon, c_lon + half_lon);
        let house = [
            (longitude - west) / (east - west),
            (north - latitude) / (north - south),
        ];
        Self {
            south,
            west,
            north,
            east,
            house,
        }
    }

    /// A grid's points, row by row from the north-west corner.
    #[must_use]
    pub fn points(&self, [nx, ny]: [usize; 2]) -> Vec<(f64, f64)> {
        let step = |from: f64, to: f64, n: usize, i: usize| {
            #[allow(clippy::cast_precision_loss)] // a few dozen
            let f = i as f64 / (n - 1) as f64;
            from + (to - from) * f
        };
        (0..ny)
            .flat_map(|j| {
                (0..nx).map(move |i| {
                    (
                        step(self.north, self.south, ny, j),
                        step(self.west, self.east, nx, i),
                    )
                })
            })
            .collect()
    }
}

impl Weather {
    #[must_use]
    pub fn new(latitude: f64, longitude: f64, data_dir: Option<PathBuf>) -> Self {
        Self(Arc::new(Inner {
            latitude,
            longitude,
            data_dir,
            forecast: Mutex::new(None),
            map: Mutex::new(None),
            land: Mutex::new(None),
        }))
    }

    /// The next 24 hours (hour by hour) and the next 7 days, as Open-Meteo's
    /// columns (`hourly`, `daily`), times in the house's time zone.
    pub async fn forecast(&self) -> anyhow::Result<Json> {
        let (lat, lon) = (self.0.latitude, self.0.longitude);
        kept(&self.0.forecast, FORECAST_KEEP, fetch_forecast(lat, lon)).await
    }

    /// The weather map: the area, the next day's wind, clouds and rain on
    /// the weather's grid, and the land (null until it could be asked).
    pub async fn map(&self) -> anyhow::Result<Json> {
        let area = Area::around(self.0.latitude, self.0.longitude);
        let weather = kept(&self.0.map, MAP_KEEP, fetch_map(area)).await?;
        let land = self.land(&area).await;
        Ok(json!({
            "area": {
                "south": area.south, "west": area.west,
                "north": area.north, "east": area.east,
            },
            "house": area.house,
            "km": [MAP_WIDTH_KM, MAP_HEIGHT_KM],
            "weather": weather,
            "land": land.map(|elevation| json!({
                "grid": LAND_GRID,
                "elevation": *elevation,
            })),
        }))
    }

    /// The land under the map: in memory, else on disk, else asked (and
    /// written down). `None` when it cannot be had now (tried again later).
    async fn land(&self, area: &Area) -> Option<Arc<Vec<f32>>> {
        let mut land = self.0.land.lock().await;
        if let Some(l) = land.as_ref() {
            return Some(l.clone());
        }
        let key = land_key(area);
        let file = self
            .0
            .data_dir
            .as_ref()
            .map(|d| d.join("weather-land.json"));
        if let Some(file) = &file
            && let Ok(text) = tokio::fs::read_to_string(file).await
            && let Ok(saved) = serde_json::from_str::<Json>(&text)
            && saved["key"] == key
            && let Some(elevation) = elevations(&saved["elevation"])
        {
            let l = Arc::new(elevation);
            *land = Some(l.clone());
            return Some(l);
        }
        match fetch_land(area).await {
            Ok(elevation) => {
                if let Some(file) = &file {
                    let saved = json!({ "key": key, "elevation": elevation });
                    let tmp = file.with_extension("json.tmp");
                    let written = async {
                        tokio::fs::write(&tmp, saved.to_string()).await?;
                        tokio::fs::rename(&tmp, file).await
                    };
                    if let Err(e) = written.await {
                        tracing::warn!("weather land not saved: {e}");
                    }
                }
                let l = Arc::new(elevation);
                *land = Some(l.clone());
                Some(l)
            }
            Err(e) => {
                tracing::warn!("weather land: {e:#}");
                None
            }
        }
    }
}

/// A value kept `keep` long; out of date is better than nothing when the
/// service fails (kept, tried again on the next look).
async fn kept(
    slot: &Mutex<Option<(Instant, Json)>>,
    keep: Duration,
    fetch: impl Future<Output = anyhow::Result<Json>>,
) -> anyhow::Result<Json> {
    let mut slot = slot.lock().await;
    if let Some((at, json)) = slot.as_ref()
        && at.elapsed() < keep
    {
        return Ok(json.clone());
    }
    match fetch.await {
        Ok(json) => {
            *slot = Some((Instant::now(), json.clone()));
            Ok(json)
        }
        Err(e) => match slot.as_ref() {
            Some((_, json)) => {
                tracing::warn!("weather: {e:#}");
                Ok(json.clone())
            }
            None => Err(e),
        },
    }
}

async fn get(path: &str, max: usize) -> anyhow::Result<Json> {
    let request = Request::builder()
        .method(Method::GET)
        .uri(path)
        .header("accept", "application/json")
        .body(Full::new(Bytes::new()))?;
    let (status, body) = moli_net::web(HOST, 443, true, request, LIMIT, max).await?;
    anyhow::ensure!(status.is_success(), "{HOST}: HTTP {}", status.as_u16());
    Ok(serde_json::from_slice(&body)?)
}

async fn fetch_forecast(latitude: f64, longitude: f64) -> anyhow::Result<Json> {
    let path = format!(
        "/v1/forecast?latitude={latitude:.2}&longitude={longitude:.2}\
         &hourly=temperature_2m,weather_code,precipitation_probability,precipitation,\
         cloud_cover,wind_speed_10m,is_day\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,\
         precipitation_probability_max,wind_speed_10m_max,sunrise,sunset\
         &forecast_hours=25&forecast_days=7&timezone=auto"
    );
    let w = get(&path, MAX_FORECAST).await?;
    anyhow::ensure!(w["hourly"]["time"].is_array(), "open-meteo: no hours");
    Ok(json!({
        "timezone": w["timezone"],
        "utc_offset_seconds": w["utc_offset_seconds"],
        "hourly": w["hourly"],
        "daily": w["daily"],
    }))
}

fn coordinates(points: &[(f64, f64)]) -> (String, String) {
    let lats: Vec<String> = points.iter().map(|(la, _)| format!("{la:.3}")).collect();
    let lons: Vec<String> = points.iter().map(|(_, lo)| format!("{lo:.3}")).collect();
    (lats.join(","), lons.join(","))
}

/// One decimal is plenty for a picture (and halves the answer).
fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// The next day on the weather's grid: `hours` (local), then per variable
/// one flat array, `[hour × points + point]`; the wind as east (`u`) and
/// north (`v`) speeds in km/h.
async fn fetch_map(area: Area) -> anyhow::Result<Json> {
    let points = area.points(WEATHER_GRID);
    let (lats, lons) = coordinates(&points);
    let path = format!(
        "/v1/forecast?latitude={lats}&longitude={lons}\
         &hourly=wind_speed_10m,wind_direction_10m,cloud_cover,precipitation\
         &forecast_hours={MAP_HOURS}&timezone=auto"
    );
    let answer = get(&path, MAX_MAP).await?;
    let places = answer.as_array().map(Vec::as_slice).unwrap_or_default();
    anyhow::ensure!(
        places.len() == points.len(),
        "open-meteo: {} places",
        places.len()
    );
    let hours: Vec<Json> = places[0]["hourly"]["time"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let n = points.len();
    let (mut u, mut v, mut cloud, mut rain) = (
        vec![0.0; hours.len() * n],
        vec![0.0; hours.len() * n],
        vec![0.0; hours.len() * n],
        vec![0.0; hours.len() * n],
    );
    for (p, place) in places.iter().enumerate() {
        let h = &place["hourly"];
        let at = |key: &str, i: usize| h[key][i].as_f64().unwrap_or(0.0);
        for i in 0..hours.len() {
            let speed = at("wind_speed_10m", i);
            // The direction the wind comes from: it blows the other way.
            let from = at("wind_direction_10m", i).to_radians();
            u[i * n + p] = round1(-speed * from.sin());
            v[i * n + p] = round1(-speed * from.cos());
            cloud[i * n + p] = at("cloud_cover", i).round();
            rain[i * n + p] = round1(at("precipitation", i));
        }
    }
    Ok(json!({
        "grid": WEATHER_GRID,
        "hours": hours,
        "utc_offset_seconds": places[0]["utc_offset_seconds"],
        "u": u, "v": v, "cloud": cloud, "rain": rain,
    }))
}

fn land_key(area: &Area) -> String {
    format!(
        "{:.3},{:.3},{MAP_WIDTH_KM},{MAP_HEIGHT_KM},{}x{}",
        area.north, area.west, LAND_GRID[0], LAND_GRID[1]
    )
}

fn elevations(json: &Json) -> Option<Vec<f32>> {
    #[allow(clippy::cast_possible_truncation)] // metres
    let v: Vec<f32> = json
        .as_array()?
        .iter()
        .map(|e| e.as_f64().map(|m| m as f32))
        .collect::<Option<_>>()?;
    (v.len() == LAND_GRID[0] * LAND_GRID[1]).then_some(v)
}

/// The land's elevations (metres, 0 at sea), a hundred points a request,
/// a few at a time.
async fn fetch_land(area: &Area) -> anyhow::Result<Vec<f32>> {
    let chunks: Vec<Vec<(f64, f64)>> = area
        .points(LAND_GRID)
        .chunks(ELEVATIONS_PER_CALL)
        .map(<[_]>::to_vec)
        .collect();
    let parts: Vec<Vec<f32>> = futures::stream::iter(chunks)
        .map(fetch_elevations)
        .buffered(4)
        .try_collect()
        .await?;
    Ok(parts.concat())
}

async fn fetch_elevations(chunk: Vec<(f64, f64)>) -> anyhow::Result<Vec<f32>> {
    let (lats, lons) = coordinates(&chunk);
    let answer = get(
        &format!("/v1/elevation?latitude={lats}&longitude={lons}"),
        MAX_ELEVATION,
    )
    .await?;
    let part = elevation_list(&answer["elevation"]);
    anyhow::ensure!(part.len() == chunk.len(), "open-meteo: elevations");
    Ok(part)
}

fn elevation_list(json: &Json) -> Vec<f32> {
    json.as_array()
        .into_iter()
        .flatten()
        .map(|e| {
            #[allow(clippy::cast_possible_truncation)] // metres
            let m = e.as_f64().unwrap_or(0.0) as f32;
            m
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_house_sits_near_the_middle_of_its_map() {
        let a = Area::around(48.86, 2.33);
        // The centre is rounded (48.9 ; 2.3): the house is a little off it.
        assert!((a.house[0] - 0.5).abs() < 0.05, "{:?}", a.house);
        assert!((a.house[1] - 0.5).abs() < 0.05, "{:?}", a.house);
        assert!(a.house[0] > 0.5, "east of the centre");
        assert!(a.house[1] > 0.5, "south of the centre");
        // 300 km tall, 400 km wide on the ground.
        assert!(((a.north - a.south) * KM_PER_DEGREE - MAP_HEIGHT_KM).abs() < 1e-6);
        let wide = (a.east - a.west) * KM_PER_DEGREE * 48.9_f64.to_radians().cos();
        assert!((wide - MAP_WIDTH_KM).abs() < 1e-6);
    }

    #[test]
    fn the_grid_runs_from_the_north_west_row_by_row() {
        let a = Area::around(48.86, 2.33);
        let p = a.points([3, 2]);
        assert_eq!(p.len(), 6);
        assert_eq!(p[0], (a.north, a.west));
        assert_eq!(p[2], (a.north, a.east));
        assert_eq!(p[5], (a.south, a.east));
    }

    #[test]
    fn saved_land_of_another_size_is_not_taken() {
        let n = LAND_GRID[0] * LAND_GRID[1];
        assert!(elevations(&json!(vec![1.0; n])).is_some());
        assert!(elevations(&json!(vec![1.0; n - 1])).is_none());
        assert!(elevations(&json!("x")).is_none());
    }
}
