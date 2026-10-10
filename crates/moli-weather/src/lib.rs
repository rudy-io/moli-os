//! The sky over the house, for the dashboard:
//!
//! - the forecast: the next hours and the days to come (Open-Meteo, free and
//!   keyless, the service the weather driver already reads);
//! - the weather map: a grid of points around the house, hour by hour for
//!   the next day (wind, clouds, rain, Open-Meteo again), and the land under
//!   it (the public « Terrain Tiles » of elevations, Terrarium PNG), from
//!   which the dashboard draws its own map: relief, clouds, rain and the
//!   wind flowing.
//!
//! Nothing is fetched until someone looks, then it is kept a while: a wall
//! of tablets costs the services one request. The land never changes: asked
//! once, kept on disk. The map is asked around a point rounded to a tenth
//! of a degree.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

const LAND_HOST: &str = "s3.amazonaws.com";
/// Zoom 7: a tile is ~230 km wide here, ~0.9 km a pixel.
pub const LAND_ZOOM: u32 = 7;
const MAX_TILE: usize = 512 * 1024;
/// After a failure, the land is asked again no sooner than this.
const LAND_RETRY: Duration = Duration::from_secs(10 * 60);

/// The map: 400 × 300 km around the house.
pub const MAP_WIDTH_KM: f64 = 400.0;
pub const MAP_HEIGHT_KM: f64 = 300.0;
/// The weather's grid (one request; ~35 km between points, about the
/// models' own mesh): the dashboard smooths between them.
pub const WEATHER_GRID: [usize; 2] = [12, 9];
/// The next day, hour by hour.
const MAP_HOURS: u32 = 24;
const KM_PER_DEGREE: f64 = 111.32;

#[derive(Clone, Debug)]
pub struct Weather(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    latitude: f64,
    longitude: f64,
    /// Where the land's tiles are kept (`weather-land/`), when there is a
    /// disk.
    data_dir: Option<PathBuf>,
    forecast: Mutex<Option<(Instant, Json)>>,
    map: Mutex<Option<(Instant, Json)>>,
    land: Mutex<Land>,
}

#[derive(Debug, Default)]
struct Land {
    /// (x, y) → PNG, once all are there.
    tiles: BTreeMap<(u32, u32), Bytes>,
    next_try: Option<Instant>,
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
        // Asked around a rounded point (~10 km): the services never learn
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

    /// The land tiles covering the area: columns `x0..=x1`, rows `y0..=y1`.
    #[must_use]
    pub fn tiles(&self) -> [u32; 4] {
        let (x0, y0) = tile(self.north, self.west);
        let (x1, y1) = tile(self.south, self.east);
        [x0, y0, x1, y1]
    }
}

/// The Web Mercator tile holding a point, at [`LAND_ZOOM`].
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..128
fn tile(latitude: f64, longitude: f64) -> (u32, u32) {
    let n = f64::from(1_u32 << LAND_ZOOM);
    let x = (longitude + 180.0) / 360.0 * n;
    let lat = latitude.to_radians();
    let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    (
        x.floor().clamp(0.0, n - 1.0) as u32,
        y.floor().clamp(0.0, n - 1.0) as u32,
    )
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
            land: Mutex::new(Land::default()),
        }))
    }

    /// The next 24 hours (hour by hour) and the next 7 days, as Open-Meteo's
    /// columns (`hourly`, `daily`), times in the house's time zone.
    pub async fn forecast(&self) -> anyhow::Result<Json> {
        let (lat, lon) = (self.0.latitude, self.0.longitude);
        kept(&self.0.forecast, FORECAST_KEEP, fetch_forecast(lat, lon)).await
    }

    /// The weather map: the area, the next day's wind, clouds and rain on
    /// the weather's grid, and the land's tiles (null until they are all
    /// there).
    pub async fn map(&self) -> anyhow::Result<Json> {
        let area = Area::around(self.0.latitude, self.0.longitude);
        let weather = kept(&self.0.map, MAP_KEEP, fetch_map(area)).await?;
        let [x0, y0, x1, y1] = area.tiles();
        let land = self
            .land_ready(&area)
            .await
            .then(|| json!({ "z": LAND_ZOOM, "x0": x0, "y0": y0, "x1": x1, "y1": y1 }));
        Ok(json!({
            "area": {
                "south": area.south, "west": area.west,
                "north": area.north, "east": area.east,
            },
            "house": area.house,
            "km": [MAP_WIDTH_KM, MAP_HEIGHT_KM],
            "weather": weather,
            "land": land,
        }))
    }

    /// One land tile of the map (PNG), only those of the house's area.
    pub async fn land_tile(&self, z: u32, x: u32, y: u32) -> Option<Bytes> {
        if z != LAND_ZOOM {
            return None;
        }
        let land = self.0.land.lock().await;
        land.tiles.get(&(x, y)).cloned()
    }

    /// Whether the land's tiles are all at hand: from memory, else from the
    /// disk, else asked (and written down).
    async fn land_ready(&self, area: &Area) -> bool {
        let [x0, y0, x1, y1] = area.tiles();
        let wanted: Vec<(u32, u32)> = (y0..=y1)
            .flat_map(|y| (x0..=x1).map(move |x| (x, y)))
            .collect();
        let mut land = self.0.land.lock().await;
        if wanted.iter().all(|t| land.tiles.contains_key(t)) {
            return true;
        }
        if land.next_try.is_some_and(|at| Instant::now() < at) {
            return false;
        }
        let dir = self.0.data_dir.as_ref().map(|d| d.join("weather-land"));
        for &(x, y) in &wanted {
            if land.tiles.contains_key(&(x, y)) {
                continue;
            }
            let file = dir
                .as_ref()
                .map(|d| d.join(format!("{LAND_ZOOM}-{x}-{y}.png")));
            if let Some(file) = &file
                && let Ok(png) = tokio::fs::read(file).await
                && png.starts_with(b"\x89PNG")
            {
                land.tiles.insert((x, y), Bytes::from(png));
                continue;
            }
            match fetch_tile(x, y).await {
                Ok(png) => {
                    if let (Some(dir), Some(file)) = (&dir, &file) {
                        let saved = async {
                            tokio::fs::create_dir_all(dir).await?;
                            let tmp = file.with_extension("tmp");
                            tokio::fs::write(&tmp, &png).await?;
                            tokio::fs::rename(&tmp, file).await
                        };
                        if let Err(e) = saved.await {
                            tracing::warn!("weather land not saved: {e}");
                        }
                    }
                    land.tiles.insert((x, y), png);
                }
                Err(e) => {
                    tracing::warn!("weather land: {e:#}");
                    land.next_try = Some(Instant::now() + LAND_RETRY);
                    return false;
                }
            }
        }
        true
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

async fn get(host: &str, path: &str, max: usize) -> anyhow::Result<Bytes> {
    let request = Request::builder()
        .method(Method::GET)
        .uri(path)
        .body(Full::new(Bytes::new()))?;
    let (status, body) = moli_net::web(host, 443, true, request, LIMIT, max).await?;
    anyhow::ensure!(status.is_success(), "{host}: HTTP {}", status.as_u16());
    Ok(body)
}

async fn get_json(path: &str, max: usize) -> anyhow::Result<Json> {
    Ok(serde_json::from_slice(&get(HOST, path, max).await?)?)
}

async fn fetch_tile(x: u32, y: u32) -> anyhow::Result<Bytes> {
    let path = format!("/elevation-tiles-prod/terrarium/{LAND_ZOOM}/{x}/{y}.png");
    let png = get(LAND_HOST, &path, MAX_TILE).await?;
    anyhow::ensure!(png.starts_with(b"\x89PNG"), "{LAND_HOST}: not a PNG");
    Ok(png)
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
    let w = get_json(&path, MAX_FORECAST).await?;
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
    let answer = get_json(&path, MAX_MAP).await?;
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
    fn the_land_tiles_cover_the_area() {
        // Zoom 7 around a point in northern France: 2 or 3 tiles a side.
        assert_eq!(tile(48.86, 2.33), (64, 44));
        let [x0, y0, x1, y1] = Area::around(48.86, 2.33).tiles();
        assert!(x0 < 64 && x1 > 64, "{x0}..{x1}");
        assert!(y0 <= 44 && y1 >= 44, "{y0}..{y1}");
        assert!((x1 - x0 + 1) * (y1 - y0 + 1) <= 9);
    }
}
