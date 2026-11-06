//! A new device's interview: who it is, what it can do, then what it should
//! report. A suite of requests over a [`Link`], no access to the hub: its
//! result is a [`DeviceRecord`] the caller publishes.
//!
//! The order follows zigbee-herdsman (MIT), `src/controller/model/device.ts`
//! (`interviewInternal`): node descriptor (often lost the first time, right
//! after a join), endpoints, simple descriptors, then `Basic` attribute by
//! attribute (some devices answer nothing to a longer read), and the IAS
//! enrollment. A silence anywhere fails it, what it learnt until then kept:
//! a sleepy device only listens for moments, and the next try goes on from
//! there. A device answering that it does not have something is an answer.

use std::time::Duration;

use anyhow::{bail, ensure};

use crate::devices;
use crate::exchange::{Link, zcl_frame};
use crate::store::{ColorCaps, DeviceRecord, EndpointRecord, Ratio};
use crate::zcl::{self, Attribute, Data};
use crate::zdo::{self, BindTarget};

/// How many times a descriptor is asked for.
const DESCRIPTOR_TRIES: usize = 3;
/// How many times an attribute is asked for.
const READ_TRIES: usize = 2;

/// `Basic`'s attributes.
const MANUFACTURER_NAME: u16 = 0x0004;
const MODEL_ID: u16 = 0x0005;
const SW_BUILD_ID: u16 = 0x4000;

/// `zoneState`: enrolled.
const ENROLLED: u64 = 1;
/// The zone number given to every sensor (herdsman's).
pub const ZONE_ID: u8 = 23;
/// What herdsman leaves a sensor between the steps of its enrollment.
const ENROLL_PAUSE: Duration = if cfg!(test) {
    Duration::from_millis(1)
} else {
    Duration::from_millis(500)
};
/// How many times it looks whether the sensor is enrolled.
const ENROLL_CHECKS: usize = 20;

/// A failed step of an interview: which, and why.
fn step(what: &str, error: &anyhow::Error) -> anyhow::Error {
    anyhow::anyhow!("{what}: {error}")
}

/// Asks up to [`DESCRIPTOR_TRIES`] times; the last failure if none answers.
async fn descriptor(
    link: &Link,
    nwk: u16,
    request: u16,
    response: u16,
    build: impl Fn(u8) -> Vec<u8> + Send + Sync,
) -> anyhow::Result<Vec<u8>> {
    let mut last = None;
    for attempt in 1..=DESCRIPTOR_TRIES {
        match link.zdo(nwk, request, response, &build).await {
            Ok(payload) => return Ok(payload),
            Err(error) => {
                tracing::debug!(
                    nwk = format_args!("{nwk:#06x}"),
                    request = format_args!("{request:#06x}"),
                    attempt,
                    %error,
                    "a descriptor request failed"
                );
                last = Some(error);
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("not asked")))
}

/// Reads attributes, up to [`READ_TRIES`] asked. An error is a silence.
async fn read(
    link: &Link,
    nwk: u16,
    endpoint: u8,
    cluster: u16,
    attributes: &[u16],
) -> anyhow::Result<Vec<Attribute>> {
    let mut last = None;
    for _ in 0..READ_TRIES {
        match link.read(nwk, endpoint, cluster, attributes).await {
            Ok(answered) => return Ok(answered),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("not asked")))
}

fn find(attributes: &[Attribute], id: u16) -> Option<Data> {
    attributes
        .iter()
        .find(|attribute| attribute.id == id)
        .map(|attribute| attribute.value.clone())
}

fn uint(attributes: &[Attribute], id: u16) -> Option<u64> {
    find(attributes, id).and_then(|data| data.as_uint())
}

fn uint16(attributes: &[Attribute], id: u16) -> Option<u16> {
    uint(attributes, id).and_then(|value| u16::try_from(value).ok())
}

fn text(data: Option<Data>) -> Option<String> {
    match data {
        Some(Data::Text(text)) if !text.is_empty() => Some(text),
        _ => None,
    }
}

/// The endpoint of `record` with `cluster` among its server clusters.
fn endpoint_with(record: &DeviceRecord, cluster: u16) -> Option<u8> {
    record
        .endpoints
        .iter()
        .find(|endpoint| endpoint.in_clusters.contains(&cluster))
        .map(|endpoint| endpoint.endpoint)
}

/// An interview that stopped: what it learnt until then, and why it stopped.
#[derive(Debug)]
pub struct Failed {
    pub record: DeviceRecord,
    pub error: anyhow::Error,
}

/// Interviews the device at `nwk`, going on from what `record` holds of it:
/// the same record, `interviewed`, with what the device said. `coordinator`
/// is the IEEE address as it travels, for a sensor to enroll with.
///
/// # Errors
///
/// A silence, or a sensor that cannot be enrolled: not interviewed, to be
/// tried again from there.
pub async fn interview(
    link: &Link,
    nwk: u16,
    mut record: DeviceRecord,
    coordinator: [u8; 8],
) -> Result<DeviceRecord, Box<Failed>> {
    match steps(link, nwk, &mut record, coordinator).await {
        Ok(()) => {
            record.interviewed = true;
            Ok(record)
        }
        Err(error) => Err(Box::new(Failed { record, error })),
    }
}

async fn steps(
    link: &Link,
    nwk: u16,
    record: &mut DeviceRecord,
    coordinator: [u8; 8],
) -> anyhow::Result<()> {
    if record.capability.is_none() {
        read_node(link, nwk, record).await?;
    }
    if record.endpoints.is_empty() {
        read_endpoints(link, nwk, record).await?;
    }
    if record.model.is_none() && record.manufacturer.is_none() {
        read_basic(link, nwk, record).await?;
    }
    if record.color.is_none()
        && let Some(endpoint) = endpoint_with(record, zcl::COLOR)
    {
        record.color = Some(read_color(link, nwk, endpoint).await?);
    }
    if let Some(endpoint) = endpoint_with(record, zcl::IAS_ZONE) {
        if record.zone_type.is_none() {
            let zone = read(link, nwk, endpoint, zcl::IAS_ZONE, &[devices::ZONE_TYPE])
                .await
                .map_err(|error| step("zone type", &error))?;
            record.zone_type = uint16(&zone, devices::ZONE_TYPE);
        }
        enroll(link, nwk, endpoint, coordinator)
            .await
            .map_err(|error| step("enrollment", &error))?;
    }
    read_scale(link, nwk, record).await
}

async fn read_node(link: &Link, nwk: u16, record: &mut DeviceRecord) -> anyhow::Result<()> {
    let node = descriptor(
        link,
        nwk,
        zdo::NODE_DESCRIPTOR_REQUEST,
        zdo::NODE_DESCRIPTOR_RESPONSE,
        |seq| zdo::node_descriptor_request(seq, nwk),
    )
    .await
    .map_err(|error| step("node descriptor", &error))?;
    let node = zdo::NodeDescriptor::parse(&node)?;
    record.logical_type = Some(node.logical_type);
    record.capability = Some(node.capability);
    record.manufacturer_code = Some(node.manufacturer_code);
    Ok(())
}

async fn read_endpoints(link: &Link, nwk: u16, record: &mut DeviceRecord) -> anyhow::Result<()> {
    let active = descriptor(
        link,
        nwk,
        zdo::ACTIVE_ENDPOINTS_REQUEST,
        zdo::ACTIVE_ENDPOINTS_RESPONSE,
        |seq| zdo::active_endpoints_request(seq, nwk),
    )
    .await
    .map_err(|error| step("active endpoints", &error))?;
    let active = zdo::ActiveEndpoints::parse(&active)?;
    ensure!(!active.endpoints.is_empty(), "no endpoint yet");
    let mut endpoints = Vec::with_capacity(active.endpoints.len());
    for endpoint in active.endpoints {
        let simple = descriptor(
            link,
            nwk,
            zdo::SIMPLE_DESCRIPTOR_REQUEST,
            zdo::SIMPLE_DESCRIPTOR_RESPONSE,
            |seq| zdo::simple_descriptor_request(seq, nwk, endpoint),
        )
        .await
        .map_err(|error| step("simple descriptor", &error))?;
        let simple = zdo::SimpleDescriptor::parse(&simple)?;
        endpoints.push(EndpointRecord {
            endpoint: simple.endpoint,
            profile: simple.profile,
            device_id: simple.device_id,
            in_clusters: simple.in_clusters,
            out_clusters: simple.out_clusters,
        });
    }
    record.endpoints = endpoints;
    Ok(())
}

/// `Basic`, attribute by attribute. One attribute missing is fine; a device
/// that answers none of them is not there yet.
async fn read_basic(link: &Link, nwk: u16, record: &mut DeviceRecord) -> anyhow::Result<()> {
    let Some(endpoint) = endpoint_with(record, zcl::BASIC) else {
        tracing::info!(device = %record.ieee, "no Basic cluster: nothing says what it is");
        return Ok(());
    };
    let mut answered = false;
    let mut last = None;
    for id in [MODEL_ID, MANUFACTURER_NAME, SW_BUILD_ID] {
        match read(link, nwk, endpoint, zcl::BASIC, &[id]).await {
            Ok(attributes) => {
                answered = true;
                let value = text(find(&attributes, id));
                match id {
                    MODEL_ID => record.model = value,
                    MANUFACTURER_NAME => record.manufacturer = value,
                    _ => record.sw_build = value,
                }
            }
            Err(error) => last = Some(error),
        }
    }
    match (answered, last) {
        (false, Some(error)) => Err(step("Basic", &error)),
        _ => Ok(()),
    }
}

async fn read_color(link: &Link, nwk: u16, endpoint: u8) -> anyhow::Result<ColorCaps> {
    let wanted = [
        devices::COLOR_CAPABILITIES,
        devices::COLOR_TEMP_MIN,
        devices::COLOR_TEMP_MAX,
    ];
    let attributes = read(link, nwk, endpoint, zcl::COLOR, &wanted)
        .await
        .map_err(|error| step("colour capabilities", &error))?;
    Ok(devices::color_caps(
        uint(&attributes, devices::COLOR_CAPABILITIES),
        uint16(&attributes, devices::COLOR_TEMP_MIN),
        uint16(&attributes, devices::COLOR_TEMP_MAX),
    ))
}

/// A sensor told where to send its alarms, then enrolled. There are two
/// procedures: it asks (the fleet accepts when the request comes), or it
/// waits to be told (the response sent here); herdsman does both, and looks
/// whether it worked: a sensor that is not enrolled sends nothing, and would
/// look fine for ever.
async fn enroll(link: &Link, nwk: u16, endpoint: u8, coordinator: [u8; 8]) -> anyhow::Result<()> {
    let enrolled =
        |attributes: &[Attribute]| uint(attributes, devices::ZONE_STATE) == Some(ENROLLED);
    let before = read(
        link,
        nwk,
        endpoint,
        zcl::IAS_ZONE,
        &[devices::IAS_CIE_ADDRESS, devices::ZONE_STATE],
    )
    .await?;
    let ours = uint(&before, devices::IAS_CIE_ADDRESS) == Some(u64::from_le_bytes(coordinator));
    if ours && enrolled(&before) {
        return Ok(());
    }
    let write = zcl::write_attribute(devices::IAS_CIE_ADDRESS, zcl::IEEE_ADDR, &coordinator);
    let answer = link.zcl(nwk, endpoint, zcl::IAS_ZONE, &write).await?;
    let accepted = zcl::Header::parse(&answer).is_ok_and(|(header, payload)| {
        header.command == zcl::WRITE_RESPONSE && zcl::all_succeeded(payload)
    });
    ensure!(accepted, "it refuses the address of its alarms");
    tokio::time::sleep(ENROLL_PAUSE).await;
    let response = zcl::enroll_response(ZONE_ID).frame(link.exchanges.next_seq());
    link.aps
        .send_unicast(nwk, &zcl_frame(endpoint, zcl::IAS_ZONE), &response)
        .await?;
    for _ in 0..ENROLL_CHECKS {
        tokio::time::sleep(ENROLL_PAUSE).await;
        let state = link
            .read(nwk, endpoint, zcl::IAS_ZONE, &[devices::ZONE_STATE])
            .await?;
        if enrolled(&state) {
            return Ok(());
        }
    }
    bail!("it does not say it is enrolled")
}

/// A ratio, `None` for a device that does not have it.
async fn ratio(
    link: &Link,
    nwk: u16,
    endpoint: u8,
    cluster: u16,
    attributes: [u16; 2],
) -> anyhow::Result<Option<Ratio>> {
    // Short reads: some devices answer nothing to a long one.
    let answered = read(link, nwk, endpoint, cluster, &attributes)
        .await
        .map_err(|error| step("ratio", &error))?;
    let value = |id| uint(&answered, id).and_then(|value| u32::try_from(value).ok());
    Ok(value(attributes[1])
        .filter(|&divisor| divisor > 0)
        .map(|divisor| Ratio {
            multiplier: value(attributes[0])
                .filter(|&multiplier| multiplier > 0)
                .unwrap_or(1),
            divisor,
        }))
}

/// What turns a meter's raw values into units: wrong, it would write
/// 12345 kWh for 12.345 in the house's history.
async fn read_scale(link: &Link, nwk: u16, record: &mut DeviceRecord) -> anyhow::Result<()> {
    if record.scale.metering.is_none()
        && let Some(endpoint) = endpoint_with(record, zcl::METERING)
    {
        let attributes = [devices::METERING_MULTIPLIER, devices::METERING_DIVISOR];
        record.scale.metering = ratio(link, nwk, endpoint, zcl::METERING, attributes).await?;
    }
    if let Some(endpoint) = endpoint_with(record, zcl::ELECTRICAL) {
        let cluster = zcl::ELECTRICAL;
        if record.scale.voltage.is_none() {
            let attributes = [devices::VOLTAGE_MULTIPLIER, devices::VOLTAGE_DIVISOR];
            record.scale.voltage = ratio(link, nwk, endpoint, cluster, attributes).await?;
        }
        if record.scale.current.is_none() {
            let attributes = [devices::CURRENT_MULTIPLIER, devices::CURRENT_DIVISOR];
            record.scale.current = ratio(link, nwk, endpoint, cluster, attributes).await?;
        }
        if record.scale.power.is_none() {
            let attributes = [devices::POWER_MULTIPLIER, devices::POWER_DIVISOR];
            record.scale.power = ratio(link, nwk, endpoint, cluster, attributes).await?;
        }
    }
    Ok(())
}

/// Binds what the device should report to the coordinator's endpoint, and
/// asks for its reports. A cluster refusing does not stop the others, and
/// counts as done: it answered. Whether every cluster answered: a silence
/// is worth another try later.
pub async fn configure(link: &Link, nwk: u16, record: &DeviceRecord, coordinator: [u8; 8]) -> bool {
    let Some(source) = crate::network::eui64(&record.ieee) else {
        tracing::warn!(device = %record.ieee, "an address that cannot be read");
        return false;
    };
    let mut answered = true;
    for binding in devices::bindings(record) {
        let bound = link
            .zdo(nwk, zdo::BIND_REQUEST, zdo::BIND_RESPONSE, |seq| {
                zdo::bind_request(
                    seq,
                    source,
                    binding.endpoint,
                    binding.cluster,
                    BindTarget::Unicast {
                        ieee: coordinator,
                        endpoint: crate::coordinator::ENDPOINT.endpoint,
                    },
                )
            })
            .await;
        match bound {
            Ok(payload) => {
                if let Err(error) = zdo::bind_response(&payload) {
                    tracing::debug!(
                        device = %record.ieee,
                        cluster = format_args!("{:#06x}", binding.cluster),
                        %error,
                        "the device refuses this binding"
                    );
                }
            }
            Err(error) => {
                tracing::debug!(device = %record.ieee, %error, "a binding was not answered");
                answered = false;
                continue;
            }
        }
        // One attribute per request: a real lamp (Innr RB 287 C) that cannot
        // report one of a list refuses the others with it.
        for report in &binding.reports {
            let request = zcl::configure_reporting(std::slice::from_ref(report));
            match link
                .zcl(nwk, binding.endpoint, binding.cluster, &request)
                .await
            {
                Ok(frame) => {
                    let accepted = zcl::Header::parse(&frame)
                        .is_ok_and(|(_, payload)| zcl::all_succeeded(payload));
                    if !accepted {
                        tracing::debug!(
                            device = %record.ieee,
                            cluster = format_args!("{:#06x}", binding.cluster),
                            attribute = format_args!("{:#06x}", report.attribute),
                            "the device refuses this report"
                        );
                    }
                }
                Err(error) => {
                    tracing::debug!(device = %record.ieee, %error, "a report was not answered");
                    answered = false;
                }
            }
        }
    }
    answered
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::aps::Aps;
    use crate::aps::tests::EMBER_13;
    use crate::ezsp::{Ezsp, ids};
    use crate::sim::{
        FakeNcp, FakeRadio, RadioRequest, lamp_answers, plug_answers, sensor_answers,
    };

    /// The IEEE address of the coordinator, as it travels.
    const COORDINATOR: [u8; 8] = [0x01, 0, 0, 0, 0, 0x4b, 0x12, 0x00];
    const NWK: u16 = 0x1234;

    /// A link over a simulated NCP, its callbacks routed as the driver does.
    async fn linked() -> (FakeNcp, Link) {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        ncp.respond(ids::SEND_UNICAST, &[0x00, 0x2A]);
        let (ezsp, version, mut callbacks): (Ezsp, _, _) = Ezsp::connect(io).await.unwrap();
        let link = Link::new(Aps::new(ezsp, version.protocol));
        let router = link.clone();
        tokio::spawn(async move {
            while let Some(callback) = callbacks.recv().await {
                if router.aps.on_message_sent(&callback) {
                    continue;
                }
                if let Ok(message) = crate::aps::Incoming::decode(&callback.params, 13) {
                    router.exchanges.deliver(&message);
                }
            }
        });
        (ncp, link)
    }

    fn fresh() -> DeviceRecord {
        DeviceRecord {
            ieee: "0x00124b0000000002".into(),
            nwk: Some(NWK),
            ..DeviceRecord::default()
        }
    }

    /// Whether a request is a read of `id` first on `cluster`.
    fn reads_first(request: &RadioRequest, cluster: u16, id: u16) -> bool {
        request.cluster == cluster
            && request.profile != zdo::PROFILE
            && request.payload[2] == 0x00
            && request.payload[3..5] == id.to_le_bytes()
    }

    #[tokio::test]
    async fn a_colour_lamp_says_what_it_is() {
        let (ncp, link) = linked().await;
        let _radio = FakeRadio::spawn(&ncp, lamp_answers());
        let record = interview(&link, NWK, fresh(), COORDINATOR).await.unwrap();
        assert!(record.interviewed);
        assert_eq!(record.model.as_deref(), Some("Some lamp"));
        assert_eq!(record.manufacturer.as_deref(), Some("Some maker"));
        assert_eq!(record.sw_build.as_deref(), Some("1.2.3"));
        assert_eq!(record.logical_type, Some(1));
        assert_eq!(record.capability, Some(0x8E));
        assert_eq!(record.manufacturer_code, Some(0x1166));
        assert_eq!(record.endpoints.len(), 1);
        assert_eq!(record.endpoints[0].endpoint, 1);
        assert_eq!(record.endpoints[0].profile, 0x0104);
        assert!(record.endpoints[0].in_clusters.contains(&zcl::COLOR));
        let caps = record.color.unwrap();
        assert!(caps.xy && caps.temp);
        assert_eq!((caps.temp_min, caps.temp_max), (Some(153), Some(556)));
        assert!(!record.configured, "configured apart");
    }

    #[tokio::test(start_paused = true)]
    async fn a_lost_first_request_is_asked_again() {
        let (ncp, link) = linked().await;
        let radio = FakeRadio::spawn(&ncp, lamp_answers());
        radio.lose_answers(1);
        let record = interview(&link, NWK, fresh(), COORDINATOR).await.unwrap();
        assert!(record.interviewed);
        assert_eq!(record.model.as_deref(), Some("Some lamp"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_that_never_answers_is_not_interviewed() {
        let (ncp, link) = linked().await;
        let radio = FakeRadio::spawn(&ncp, lamp_answers());
        radio.lose_answers(usize::MAX);
        let failed = interview(&link, NWK, fresh(), COORDINATOR)
            .await
            .unwrap_err();
        assert!(failed.error.to_string().contains("node descriptor"));
        assert!(!failed.record.interviewed);
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_basic_is_a_failure_and_the_next_try_goes_on_from_there() {
        let (ncp, link) = linked().await;
        let radio = FakeRadio::spawn(&ncp, |request| {
            if request.cluster == zcl::BASIC {
                None
            } else {
                lamp_answers()(request)
            }
        });
        let failed = interview(&link, NWK, fresh(), COORDINATOR)
            .await
            .unwrap_err();
        assert!(failed.error.to_string().contains("Basic"));
        assert_eq!(failed.record.capability, Some(0x8E), "what it learnt stays");
        assert_eq!(failed.record.endpoints.len(), 1);
        // A sleepy device awake again, for a moment.
        radio.answer_with(lamp_answers());
        let before = radio.requests().len();
        let record = interview(&link, NWK, failed.record, COORDINATOR)
            .await
            .unwrap();
        assert!(record.interviewed);
        assert_eq!(record.model.as_deref(), Some("Some lamp"));
        assert!(
            radio.requests()[before..]
                .iter()
                .all(|request| request.profile != zdo::PROFILE),
            "no descriptor asked again"
        );
    }

    #[tokio::test]
    async fn a_missing_attribute_is_not_a_failure() {
        let (ncp, link) = linked().await;
        let _radio = FakeRadio::spawn(&ncp, |request| {
            if reads_first(request, zcl::BASIC, SW_BUILD_ID) {
                // UNSUPPORTED_ATTRIBUTE
                let mut frame = vec![0x18, request.payload[1], zcl::READ_RESPONSE];
                frame.extend_from_slice(&SW_BUILD_ID.to_le_bytes());
                frame.push(0x86);
                Some(frame)
            } else {
                lamp_answers()(request)
            }
        });
        let record = interview(&link, NWK, fresh(), COORDINATOR).await.unwrap();
        assert!(record.interviewed);
        assert_eq!(record.sw_build, None);
        assert_eq!(record.model.as_deref(), Some("Some lamp"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_silence_about_its_colours_is_not_taken_for_an_answer() {
        let (ncp, link) = linked().await;
        let radio = FakeRadio::spawn(&ncp, |request| {
            if reads_first(request, zcl::COLOR, devices::COLOR_CAPABILITIES) {
                None
            } else {
                lamp_answers()(request)
            }
        });
        let failed = interview(&link, NWK, fresh(), COORDINATOR)
            .await
            .unwrap_err();
        assert!(failed.error.to_string().contains("colour"));
        assert_eq!(failed.record.color, None, "not guessed");
        assert_eq!(failed.record.model.as_deref(), Some("Some lamp"));
        // A lamp that says it reads no such thing: an answer, and the
        // defaults hold.
        radio.answer_with(|request| {
            if reads_first(request, zcl::COLOR, devices::COLOR_CAPABILITIES) {
                Some(vec![
                    0x18,
                    request.payload[1],
                    zcl::DEFAULT_RESPONSE,
                    0x00,
                    0x82,
                ])
            } else {
                lamp_answers()(request)
            }
        });
        let record = interview(&link, NWK, failed.record, COORDINATOR)
            .await
            .unwrap();
        let caps = record.color.unwrap();
        assert!(caps.xy && caps.temp);
    }

    #[tokio::test]
    async fn a_sensor_is_told_where_to_send_its_alarms() {
        let (ncp, link) = linked().await;
        let radio = FakeRadio::spawn(&ncp, sensor_answers());
        let record = interview(&link, NWK, fresh(), COORDINATOR).await.unwrap();
        assert_eq!(record.zone_type, Some(0x0015));
        assert!(!record.mains() && !record.listening());
        let write = radio
            .requests()
            .into_iter()
            .find(|request| request.cluster == zcl::IAS_ZONE && request.payload[2] == 0x02)
            .expect("the address written");
        assert_eq!(write.payload[3..6], [0x10, 0x00, zcl::IEEE_ADDR]);
        assert_eq!(write.payload[6..], COORDINATOR);
    }

    /// A sensor that only enrolls once told to, without asking (auto enroll).
    fn waiting_to_be_told(request: &RadioRequest, told: &AtomicBool) -> Option<Vec<u8>> {
        if request.cluster == zcl::IAS_ZONE && request.payload[0] == 0x11 {
            assert_eq!(request.payload[2..], [0x00, 0x00, ZONE_ID]);
            told.store(true, Ordering::SeqCst);
            return None;
        }
        if reads_first(request, zcl::IAS_ZONE, devices::ZONE_STATE)
            || reads_first(request, zcl::IAS_ZONE, devices::IAS_CIE_ADDRESS)
        {
            let state = u8::from(told.load(Ordering::SeqCst));
            let mut frame = vec![0x18, request.payload[1], zcl::READ_RESPONSE];
            frame.extend_from_slice(&devices::ZONE_STATE.to_le_bytes());
            frame.extend_from_slice(&[0x00, zcl::ENUM8, state]);
            return Some(frame);
        }
        sensor_answers()(request)
    }

    #[tokio::test]
    async fn a_sensor_waiting_to_be_told_is_enrolled() {
        let (ncp, link) = linked().await;
        let told = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&told);
        let _radio = FakeRadio::spawn(&ncp, move |request| waiting_to_be_told(request, &seen));
        let record = interview(&link, NWK, fresh(), COORDINATOR).await.unwrap();
        assert!(record.interviewed);
        assert!(told.load(Ordering::SeqCst), "the response was sent");
    }

    #[tokio::test(start_paused = true)]
    async fn a_sensor_that_does_not_enroll_is_not_interviewed() {
        let (ncp, link) = linked().await;
        let _radio = FakeRadio::spawn(&ncp, |request| {
            if reads_first(request, zcl::IAS_ZONE, devices::ZONE_STATE)
                || reads_first(request, zcl::IAS_ZONE, devices::IAS_CIE_ADDRESS)
            {
                let mut frame = vec![0x18, request.payload[1], zcl::READ_RESPONSE];
                frame.extend_from_slice(&devices::ZONE_STATE.to_le_bytes());
                frame.extend_from_slice(&[0x00, zcl::ENUM8, 0]);
                Some(frame)
            } else {
                sensor_answers()(request)
            }
        });
        let failed = interview(&link, NWK, fresh(), COORDINATOR)
            .await
            .unwrap_err();
        assert!(failed.error.to_string().contains("enrollment"));
        assert_eq!(
            failed.record.zone_type,
            Some(0x0015),
            "what it learnt stays"
        );
    }

    #[tokio::test]
    async fn a_sensor_refusing_the_address_is_not_interviewed() {
        let (ncp, link) = linked().await;
        let _radio = FakeRadio::spawn(&ncp, |request| {
            if request.cluster == zcl::IAS_ZONE && request.payload[2] == 0x02 {
                Some(vec![
                    0x18,
                    request.payload[1],
                    zcl::WRITE_RESPONSE,
                    0x88,
                    0x10,
                    0x00,
                ])
            } else {
                sensor_answers()(request)
            }
        });
        let failed = interview(&link, NWK, fresh(), COORDINATOR)
            .await
            .unwrap_err();
        assert!(failed.error.to_string().contains("refuses"));
    }

    #[tokio::test]
    async fn a_sensor_already_enrolled_with_us_is_left_alone() {
        let (ncp, link) = linked().await;
        let radio = FakeRadio::spawn(&ncp, |request| {
            if reads_first(request, zcl::IAS_ZONE, devices::IAS_CIE_ADDRESS) {
                let mut frame = vec![0x18, request.payload[1], zcl::READ_RESPONSE];
                frame.extend_from_slice(&devices::IAS_CIE_ADDRESS.to_le_bytes());
                frame.extend_from_slice(&[0x00, zcl::IEEE_ADDR]);
                frame.extend_from_slice(&COORDINATOR);
                frame.extend_from_slice(&devices::ZONE_STATE.to_le_bytes());
                frame.extend_from_slice(&[0x00, zcl::ENUM8, 1]);
                Some(frame)
            } else {
                sensor_answers()(request)
            }
        });
        let record = interview(&link, NWK, fresh(), COORDINATOR).await.unwrap();
        assert!(record.interviewed);
        assert!(
            radio
                .requests()
                .iter()
                .all(|request| request.payload[2] != 0x02),
            "nothing written"
        );
    }

    #[tokio::test]
    async fn a_plug_says_how_to_read_its_meter() {
        let (ncp, link) = linked().await;
        let _radio = FakeRadio::spawn(&ncp, plug_answers());
        let record = interview(&link, NWK, fresh(), COORDINATOR).await.unwrap();
        let thousandth = Some(Ratio {
            multiplier: 1,
            divisor: 1000,
        });
        let tenth = Some(Ratio {
            multiplier: 1,
            divisor: 10,
        });
        assert_eq!(record.scale.metering, thousandth);
        assert_eq!(record.scale.voltage, tenth);
        assert_eq!(record.scale.current, thousandth);
        assert_eq!(record.scale.power, tenth);
    }

    #[tokio::test(start_paused = true)]
    async fn a_silence_about_its_meter_is_not_taken_for_an_answer() {
        let (ncp, link) = linked().await;
        let _radio = FakeRadio::spawn(&ncp, |request| {
            if reads_first(request, zcl::ELECTRICAL, devices::POWER_MULTIPLIER) {
                None
            } else {
                plug_answers()(request)
            }
        });
        let failed = interview(&link, NWK, fresh(), COORDINATOR)
            .await
            .unwrap_err();
        assert!(failed.error.to_string().contains("ratio"));
        assert!(
            failed.record.scale.voltage.is_some(),
            "what it learnt stays"
        );
        assert!(!failed.record.interviewed);
    }

    #[tokio::test]
    async fn configuring_binds_then_asks_for_reports() {
        let (ncp, link) = linked().await;
        let radio = FakeRadio::spawn(&ncp, lamp_answers());
        let record = interview(&link, NWK, fresh(), COORDINATOR).await.unwrap();
        let before = radio.requests().len();
        assert!(configure(&link, NWK, &record, COORDINATOR).await);
        let sent = radio.requests()[before..].to_vec();
        let clusters: Vec<_> = sent.iter().map(|request| request.cluster).collect();
        assert_eq!(
            clusters,
            [
                zdo::BIND_REQUEST,
                zcl::ON_OFF,
                zdo::BIND_REQUEST,
                zcl::LEVEL,
                zdo::BIND_REQUEST,
                zcl::COLOR,
                zcl::COLOR,
                zcl::COLOR,
                zcl::COLOR,
            ]
        );
        let mut bind = crate::network::eui64(&record.ieee).unwrap().to_vec();
        bind.extend_from_slice(&[1, 0x06, 0x00, 0x03]);
        bind.extend_from_slice(&COORDINATOR);
        bind.push(1);
        assert_eq!(sent[0].payload[1..], bind);
        assert_eq!(sent[1].payload[2], 0x06, "configure reporting");
    }

    #[tokio::test]
    async fn a_report_refused_costs_only_itself() {
        let (ncp, link) = linked().await;
        // What a real lamp answers about its colour mode.
        let radio = FakeRadio::spawn(&ncp, |request| {
            let colour_mode = request.cluster == zcl::COLOR
                && request.payload[2] == 0x06
                && request.payload[4..6] == devices::COLOR_MODE.to_le_bytes();
            if colour_mode {
                Some(vec![
                    0x18,
                    request.payload[1],
                    zcl::CONFIGURE_RESPONSE,
                    0x89,
                    0x00,
                    0x08,
                    0x00,
                ])
            } else {
                lamp_answers()(request)
            }
        });
        let record = interview(&link, NWK, fresh(), COORDINATOR).await.unwrap();
        let before = radio.requests().len();
        assert!(
            configure(&link, NWK, &record, COORDINATOR).await,
            "it answered"
        );
        let asked: Vec<_> = radio.requests()[before..]
            .iter()
            .filter(|request| request.cluster == zcl::COLOR)
            .map(|request| u16::from_le_bytes([request.payload[4], request.payload[5]]))
            .collect();
        assert_eq!(
            asked,
            [
                devices::COLOR_TEMPERATURE,
                devices::CURRENT_X,
                devices::CURRENT_Y,
                devices::COLOR_MODE,
            ],
            "what matters first, each on its own"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_silence_is_tried_again_a_refusal_is_not() {
        let (ncp, link) = linked().await;
        let radio = FakeRadio::spawn(&ncp, lamp_answers());
        let record = interview(&link, NWK, fresh(), COORDINATOR).await.unwrap();
        radio.answer_with(|request| {
            // Every binding refused (NOT_SUPPORTED), reports refused too.
            if request.cluster == zdo::BIND_REQUEST {
                Some(vec![request.payload[0], 0x84])
            } else {
                Some(vec![
                    0x18,
                    request.payload[1],
                    zcl::CONFIGURE_RESPONSE,
                    0x86,
                    0,
                    0,
                    0,
                ])
            }
        });
        assert!(
            configure(&link, NWK, &record, COORDINATOR).await,
            "refused: done"
        );
        radio.lose_answers(usize::MAX);
        assert!(
            !configure(&link, NWK, &record, COORDINATOR).await,
            "silent: not done"
        );
    }
}
