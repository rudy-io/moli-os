//! The sky over the house, for the dashboard:
//!
//! - the forecast: the next hours and the days to come (Open-Meteo, free and
//!   keyless, the service the weather driver already reads);
//! - the clouds seen from space: the last two hours of Meteosat images
//!   (EUMETSAT's « GeoColour », one every ten minutes, true colours by day,
//!   clouds over the lights of the towns by night), cropped around the house.
//!
//! Nothing is fetched until someone looks, then both are kept a while: a
//! wall of tablets costs the services one request each, the house no more
//! than a megabyte of images. The images are asked around a point rounded
//! to a quarter of a degree, never the house's own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use http::{Method, Request};
use http_body_util::Full;
use moli_net::Body as Bytes;
use serde_json::{Value as Json, json};
use tokio::sync::Mutex;

const FORECAST_HOST: &str = "api.open-meteo.com";
/// Open-Meteo refreshes its models every hour or so.
const FORECAST_KEEP: Duration = Duration::from_secs(15 * 60);
const FORECAST_LIMIT: Duration = Duration::from_secs(10);
const MAX_FORECAST: usize = 256 * 1024;

const SKY_HOST: &str = "view.eumetsat.int";
const SKY_LAYER: &str = "mtg_fd:rgb_geocolour";
/// The layer's own capabilities: a few kilobytes, the latest image's time.
const SKY_CAPABILITIES: &str =
    "/geoserver/mtg_fd/rgb_geocolour/ows?service=WMS&request=GetCapabilities&version=1.3.0";
/// Two hours of images, one every ten minutes.
pub const FRAMES: usize = 12;
const FRAME_STEP_MIN: i64 = 10;
/// A new image every ten minutes: looked for at most every five.
const SKY_CHECK: Duration = Duration::from_secs(5 * 60);
/// After a failure, tried again a minute later (the old images stay).
const SKY_RETRY: Duration = Duration::from_secs(60);
const SKY_LIMIT: Duration = Duration::from_secs(20);
const MAX_CAPABILITIES: usize = 64 * 1024;
const MAX_FRAME: usize = 1024 * 1024;
/// Web Mercator's sphere.
const EARTH_M: f64 = 6_378_137.0;

/// How far the images look: close around the house (the home page's
/// picture, about as close as Meteosat's kilometre allows), or the whole
/// region (the storms seen coming from far).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zoom {
    Near,
    Wide,
}

impl Zoom {
    /// Width on the ground (the height is three quarters of it).
    #[must_use]
    pub const fn width_km(self) -> f64 {
        match self {
            Self::Near => 260.0,
            Self::Wide => 840.0,
        }
    }

    /// The image's pixels (4:3): about one per kilometre for the close one,
    /// the browser smooths it.
    #[must_use]
    pub const fn size(self) -> [u32; 2] {
        match self {
            Self::Near => [480, 360],
            Self::Wide => [960, 720],
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Near => 0,
            Self::Wide => 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Weather(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    latitude: f64,
    longitude: f64,
    forecast: Mutex<Option<(Instant, Json)>>,
    /// One per [`Zoom`].
    sky: [Mutex<Sky>; 2],
}

#[derive(Debug, Default)]
struct Sky {
    next_check: Option<Instant>,
    /// Oldest first: (`2026-10-10T13:40:00Z`, JPEG).
    frames: Vec<(String, Bytes)>,
}

/// Where the region's images are taken, in Web Mercator metres, and where
/// the house sits on them (0..1 from the left, from the top).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub bbox: [f64; 4],
    pub house: [f64; 2],
}

impl Region {
    #[must_use]
    pub fn around(latitude: f64, longitude: f64, zoom: Zoom) -> Self {
        // The images are asked around a rounded point (a tenth of a degree,
        // ~10 km): the provider never learns where the house is.
        let c_lat = (latitude * 10.0).round() / 10.0;
        let c_lon = (longitude * 10.0).round() / 10.0;
        // Mercator stretches distances by 1/cos(latitude).
        let stretch = 1.0 / c_lat.to_radians().cos();
        let half_w = zoom.width_km() * 500.0 * stretch;
        let half_h = half_w * 0.75;
        let (cx, cy) = mercator(c_lat, c_lon);
        let bbox = [cx - half_w, cy - half_h, cx + half_w, cy + half_h];
        let (hx, hy) = mercator(latitude, longitude);
        let house = [
            (hx - bbox[0]) / (bbox[2] - bbox[0]),
            (bbox[3] - hy) / (bbox[3] - bbox[1]),
        ];
        Self { bbox, house }
    }
}

fn mercator(latitude: f64, longitude: f64) -> (f64, f64) {
    let x = EARTH_M * longitude.to_radians();
    let y = EARTH_M
        * (std::f64::consts::FRAC_PI_4 + latitude.to_radians() / 2.0)
            .tan()
            .ln();
    (x, y)
}

impl Weather {
    #[must_use]
    pub fn new(latitude: f64, longitude: f64) -> Self {
        Self(Arc::new(Inner {
            latitude,
            longitude,
            forecast: Mutex::new(None),
            sky: [Mutex::new(Sky::default()), Mutex::new(Sky::default())],
        }))
    }

    /// The next 24 hours (hour by hour) and the next 7 days, as Open-Meteo's
    /// columns (`hourly`, `daily`), times in the house's time zone.
    pub async fn forecast(&self) -> anyhow::Result<Json> {
        let mut kept = self.0.forecast.lock().await;
        if let Some((at, json)) = kept.as_ref()
            && at.elapsed() < FORECAST_KEEP
        {
            return Ok(json.clone());
        }
        match fetch_forecast(self.0.latitude, self.0.longitude).await {
            Ok(json) => {
                *kept = Some((Instant::now(), json.clone()));
                Ok(json)
            }
            // Out of date is better than nothing (kept, tried again later).
            Err(e) => match kept.as_ref() {
                Some((_, json)) => {
                    tracing::warn!("forecast: {e:#}");
                    Ok(json.clone())
                }
                None => Err(e),
            },
        }
    }

    /// The images at hand (their times, oldest first) and where the house
    /// is on them; new ones are looked for when it is time.
    pub async fn sky(&self, zoom: Zoom) -> anyhow::Result<Json> {
        let region = Region::around(self.0.latitude, self.0.longitude, zoom);
        let mut sky = self.0.sky[zoom.index()].lock().await;
        if sky.next_check.is_none_or(|at| Instant::now() >= at) {
            match refresh(&mut sky, &region, zoom).await {
                Ok(()) => sky.next_check = Some(Instant::now() + SKY_CHECK),
                Err(e) => {
                    tracing::warn!("satellite: {e:#}");
                    sky.next_check = Some(Instant::now() + SKY_RETRY);
                    if sky.frames.is_empty() {
                        return Err(e);
                    }
                }
            }
        }
        Ok(json!({
            "frames": sky.frames.iter().map(|(t, _)| t).collect::<Vec<_>>(),
            "every_min": FRAME_STEP_MIN,
            "house": region.house,
            "width_km": zoom.width_km(),
            "size": zoom.size(),
            "source": "EUMETSAT · Meteosat",
        }))
    }

    /// One image already at hand (never fetched for the asking).
    pub async fn frame(&self, zoom: Zoom, time: &str) -> Option<Bytes> {
        let sky = self.0.sky[zoom.index()].lock().await;
        sky.frames
            .iter()
            .find(|(t, _)| t == time)
            .map(|(_, jpeg)| jpeg.clone())
    }
}

async fn get(host: &str, path: &str, limit: Duration, max: usize) -> anyhow::Result<Bytes> {
    let request = Request::builder()
        .method(Method::GET)
        .uri(path)
        .body(Full::new(Bytes::new()))?;
    let (status, body) = moli_net::web(host, 443, true, request, limit, max).await?;
    anyhow::ensure!(status.is_success(), "{host}: HTTP {}", status.as_u16());
    Ok(body)
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
    let body = get(FORECAST_HOST, &path, FORECAST_LIMIT, MAX_FORECAST).await?;
    let w: Json = serde_json::from_slice(&body)?;
    anyhow::ensure!(w["hourly"]["time"].is_array(), "open-meteo: no hours");
    Ok(json!({
        "timezone": w["timezone"],
        "utc_offset_seconds": w["utc_offset_seconds"],
        "hourly": w["hourly"],
        "daily": w["daily"],
    }))
}

/// The latest image's time, from the layer's capabilities.
fn latest(capabilities: &str) -> Option<jiff::Timestamp> {
    let dimension = capabilities.find("<Dimension name=\"time\"")?;
    let rest = &capabilities[dimension..];
    let start = rest.find("default=\"")? + "default=\"".len();
    let end = rest[start..].find('"')?;
    rest[start..start + end].parse().ok()
}

/// The images wanted: the last [`FRAMES`], ten minutes apart, oldest first.
fn wanted(latest: jiff::Timestamp) -> Vec<String> {
    (0..FRAMES)
        .rev()
        .filter_map(|i| {
            let back = jiff::SignedDuration::from_mins(FRAME_STEP_MIN * i64::try_from(i).ok()?);
            latest.checked_sub(back).ok()
        })
        .map(|t| t.strftime("%Y-%m-%dT%H:%M:%SZ").to_string())
        .collect()
}

async fn refresh(sky: &mut Sky, region: &Region, zoom: Zoom) -> anyhow::Result<()> {
    let caps = get(SKY_HOST, SKY_CAPABILITIES, SKY_LIMIT, MAX_CAPABILITIES).await?;
    let latest = latest(&String::from_utf8_lossy(&caps))
        .ok_or_else(|| anyhow::anyhow!("{SKY_HOST}: no image time"))?;
    let wanted = wanted(latest);
    if sky.frames.last().map(|(t, _)| t) == wanted.last() {
        return Ok(());
    }
    let mut kept: Vec<(String, Bytes)> = std::mem::take(&mut sky.frames)
        .into_iter()
        .filter(|(t, _)| wanted.contains(t))
        .collect();
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|t| !kept.iter().any(|(k, _)| k == *t))
        .collect();
    let fetched =
        futures::future::join_all(missing.iter().map(|t| fetch_frame(region, zoom, t))).await;
    for (time, image) in missing.into_iter().zip(fetched) {
        match image {
            Ok(jpeg) => kept.push((time.clone(), jpeg)),
            // A missing image is a gap in the loop, not a failure.
            Err(e) => tracing::debug!("satellite {time}: {e:#}"),
        }
    }
    kept.sort_by(|a, b| a.0.cmp(&b.0));
    sky.frames = kept;
    anyhow::ensure!(!sky.frames.is_empty(), "{SKY_HOST}: no image");
    Ok(())
}

async fn fetch_frame(region: &Region, zoom: Zoom, time: &str) -> anyhow::Result<Bytes> {
    let [x0, y0, x1, y1] = region.bbox;
    let [width, height] = zoom.size();
    let path = format!(
        "/geoserver/ows?service=WMS&version=1.3.0&request=GetMap&layers={SKY_LAYER}&styles=\
         &crs=EPSG:3857&bbox={x0:.0},{y0:.0},{x1:.0},{y1:.0}\
         &width={width}&height={height}&format=image/jpeg&time={time}"
    );
    let jpeg = get(SKY_HOST, &path, SKY_LIMIT, MAX_FRAME).await?;
    // A WMS error comes back as XML, sometimes with a 200.
    anyhow::ensure!(jpeg.starts_with(&[0xFF, 0xD8]), "not an image");
    Ok(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_house_sits_near_the_middle_of_its_region() {
        for zoom in [Zoom::Near, Zoom::Wide] {
            let r = Region::around(48.86, 2.33, zoom);
            // The centre is rounded (48.9 ; 2.3): the house is a little off it.
            assert!((r.house[0] - 0.5).abs() < 0.05, "{:?}", r.house);
            assert!((r.house[1] - 0.5).abs() < 0.05, "{:?}", r.house);
            assert!(r.house[0] > 0.5, "east of the centre");
            assert!(r.house[1] > 0.5, "south of the centre");
            // So wide on the ground, stretched by Mercator.
            let width = r.bbox[2] - r.bbox[0];
            let expected = zoom.width_km() * 1000.0 / 48.9_f64.to_radians().cos();
            assert!((width - expected).abs() < 1.0);
            assert!(((r.bbox[3] - r.bbox[1]) / width - 0.75).abs() < 1e-9);
        }
    }

    #[test]
    fn the_latest_image_is_read_from_the_capabilities() {
        let caps = r#"<Layer><Name>rgb_geocolour</Name>
            <Dimension name="time" default="2026-10-10T13:40:00Z" units="ISO8601" nearestValue="1">
            2024-09-23T00:00:00.000Z/2026-10-10T13:40:00.000Z/PT10M</Dimension></Layer>"#;
        let t = latest(caps).unwrap();
        let w = wanted(t);
        assert_eq!(w.len(), FRAMES);
        assert_eq!(w.last().unwrap(), "2026-10-10T13:40:00Z");
        assert_eq!(w[0], "2026-10-10T11:50:00Z");
        assert_eq!(latest("<WMS_Capabilities/>"), None);
    }

    #[test]
    fn the_hours_roll_over_midnight() {
        let w = wanted("2026-10-11T00:30:00Z".parse().unwrap());
        assert_eq!(w[0], "2026-10-10T22:40:00Z");
    }
}
