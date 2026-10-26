//! SNMP v2c, read only, only what a printer driver needs (RFC 3416): GET
//! and GETNEXT of a few objects, BER encoded here. Printers answer the
//! Printer MIB (RFC 3805) to the `public` community: pages printed, each
//! supply's level, the alerts in standard codes, the serial number; a
//! Brother adds its maintenance record (toner and drum in percent, where
//! the standard says only « some left »). Many printers of before IPP
//! Everywhere say their supplies only there.

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use anyhow::{Context as _, bail};
use tokio::net::UdpSocket;

const PORT: u16 = 161;
/// One answer awaited this long, asked this many times (UDP).
const WAIT: Duration = Duration::from_millis(1500);
const TRIES: usize = 2;
/// Rows read at most in a table (supplies, alerts).
const MAX_ROWS: usize = 16;

const INTEGER: u8 = 0x02;
const OCTETS: u8 = 0x04;
const NULL: u8 = 0x05;
const OID: u8 = 0x06;
const SEQUENCE: u8 = 0x30;
const COUNTER32: u8 = 0x41;
const GAUGE32: u8 = 0x42;
const TIMETICKS: u8 = 0x43;
const COUNTER64: u8 = 0x46;
const GET: u8 = 0xA0;
const GET_NEXT: u8 = 0xA1;
const RESPONSE: u8 = 0xA2;
/// SNMP v2c, as the version field says it.
const V2C: i64 = 1;

/// A value, as far as a driver reads one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    Int(i64),
    Bytes(Vec<u8>),
    Oid(Vec<u32>),
    /// `noSuchObject`, `noSuchInstance`, `endOfMibView`.
    Missing,
    /// Anything else (addresses, `NULL`).
    Other,
}

impl Val {
    #[must_use]
    pub fn int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Text as a printer writes it: trailing NULs and blanks dropped,
    /// nothing left = none.
    #[must_use]
    pub fn text(&self) -> Option<String> {
        let Self::Bytes(b) = self else { return None };
        let text = String::from_utf8_lossy(b);
        let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        (!text.is_empty()).then(|| text.to_owned())
    }

    #[must_use]
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

// ---- BER ---------------------------------------------------------------------------------

fn tlv(out: &mut Vec<u8>, tag: u8, content: &[u8]) {
    out.push(tag);
    let n = content.len();
    if n < 0x80 {
        out.push(u8::try_from(n).unwrap_or(0));
    } else if n <= 0xFF {
        out.extend_from_slice(&[0x81, u8::try_from(n).unwrap_or(0)]);
    } else {
        out.push(0x82);
        out.extend_from_slice(&u16::try_from(n).unwrap_or(u16::MAX).to_be_bytes());
    }
    out.extend_from_slice(content);
}

fn integer(out: &mut Vec<u8>, v: i64) {
    let bytes = v.to_be_bytes();
    // The shortest two's complement: a leading byte is dropped while the
    // next keeps the sign.
    let mut at = 0;
    while at < 7
        && ((bytes[at] == 0 && bytes[at + 1] < 0x80)
            || (bytes[at] == 0xFF && bytes[at + 1] >= 0x80))
    {
        at += 1;
    }
    tlv(out, INTEGER, &bytes[at..]);
}

fn oid(out: &mut Vec<u8>, arcs: &[u32]) {
    let mut body = Vec::with_capacity(arcs.len() + 4);
    let (first, rest) = match arcs {
        [a, b, rest @ ..] => (a * 40 + b, rest),
        _ => (0, &[][..]),
    };
    for arc in std::iter::once(first).chain(rest.iter().copied()) {
        let mut groups = vec![u8::try_from(arc & 0x7F).unwrap_or(0)];
        let mut v = arc >> 7;
        while v > 0 {
            groups.push(u8::try_from(v & 0x7F).unwrap_or(0) | 0x80);
            v >>= 7;
        }
        body.extend(groups.iter().rev());
    }
    tlv(out, OID, &body);
}

/// A request for `oids` (`pdu`: GET or GETNEXT).
#[must_use]
pub fn encode(id: i32, pdu: u8, community: &str, oids: &[Vec<u32>]) -> Vec<u8> {
    let mut binds = Vec::new();
    for o in oids {
        let mut bind = Vec::new();
        oid(&mut bind, o);
        tlv(&mut bind, NULL, &[]);
        tlv(&mut binds, SEQUENCE, &bind);
    }
    let mut body = Vec::new();
    integer(&mut body, i64::from(id));
    integer(&mut body, 0);
    integer(&mut body, 0);
    tlv(&mut body, SEQUENCE, &binds);
    let mut message = Vec::new();
    integer(&mut message, V2C);
    tlv(&mut message, OCTETS, community.as_bytes());
    tlv(&mut message, pdu, &body);
    let mut out = Vec::with_capacity(message.len() + 4);
    tlv(&mut out, SEQUENCE, &message);
    out
}

struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn next(&mut self) -> anyhow::Result<(u8, &'a [u8])> {
        let [tag, first, rest @ ..] = self.data else {
            bail!("SNMP: truncated answer");
        };
        let (len, rest) = if *first < 0x80 {
            (usize::from(*first), rest)
        } else {
            let n = usize::from(first & 0x7F);
            if n == 0 || n > 3 || rest.len() < n {
                bail!("SNMP: bad length");
            }
            let len = rest[..n]
                .iter()
                .fold(0usize, |l, b| (l << 8) | usize::from(*b));
            (len, &rest[n..])
        };
        if rest.len() < len {
            bail!("SNMP: truncated answer");
        }
        self.data = &rest[len..];
        Ok((*tag, &rest[..len]))
    }

    fn expect(&mut self, tag: u8) -> anyhow::Result<&'a [u8]> {
        let (got, content) = self.next()?;
        if got != tag {
            bail!("SNMP: {got:#04x} where {tag:#04x} was expected");
        }
        Ok(content)
    }
}

fn signed(content: &[u8]) -> i64 {
    let start: i64 = if content.first().is_some_and(|b| b & 0x80 != 0) {
        -1
    } else {
        0
    };
    content
        .iter()
        .take(8)
        .fold(start, |v, b| (v << 8) | i64::from(*b))
}

fn unsigned(content: &[u8]) -> i64 {
    let v = content.iter().fold(0u64, |v, b| (v << 8) | u64::from(*b));
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn arcs(content: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut v: u32 = 0;
    for b in content {
        v = (v << 7) | u32::from(b & 0x7F);
        if b & 0x80 == 0 {
            if out.is_empty() {
                out.push((v / 40).min(2));
                out.push(v - out[0] * 40);
            } else {
                out.push(v);
            }
            v = 0;
        }
    }
    out
}

/// An answer: its request id, its error status, its objects.
pub type Answer = (i32, i64, Vec<(Vec<u32>, Val)>);

/// Reads an answer.
///
/// # Errors
/// When it is not an SNMP v2c answer.
pub fn decode(data: &[u8]) -> anyhow::Result<Answer> {
    let mut message = Reader {
        data: Reader { data }.expect(SEQUENCE)?,
    };
    let version = signed(message.expect(INTEGER)?);
    if version != V2C {
        bail!("SNMP: version {version}");
    }
    message.expect(OCTETS)?;
    let mut pdu = Reader {
        data: message.expect(RESPONSE)?,
    };
    let id = i32::try_from(signed(pdu.expect(INTEGER)?)).unwrap_or(0);
    let status = signed(pdu.expect(INTEGER)?);
    pdu.expect(INTEGER)?;
    let mut binds = Reader {
        data: pdu.expect(SEQUENCE)?,
    };
    let mut out = Vec::new();
    while !binds.data.is_empty() {
        let mut bind = Reader {
            data: binds.expect(SEQUENCE)?,
        };
        let name = arcs(bind.expect(OID)?);
        let (tag, content) = bind.next()?;
        let value = match tag {
            INTEGER => Val::Int(signed(content)),
            OCTETS => Val::Bytes(content.to_vec()),
            OID => Val::Oid(arcs(content)),
            COUNTER32 | GAUGE32 | TIMETICKS | COUNTER64 => Val::Int(unsigned(content)),
            0x80..=0x82 => Val::Missing,
            _ => Val::Other,
        };
        out.push((name, value));
    }
    Ok((id, status, out))
}

// ---- asking --------------------------------------------------------------------------------

#[derive(Debug)]
pub struct Client {
    host: String,
    community: String,
    ids: AtomicI32,
}

impl Client {
    #[must_use]
    pub fn new(host: &str, community: &str) -> Self {
        Self {
            host: host.to_owned(),
            community: community.to_owned(),
            ids: AtomicI32::new(1),
        }
    }

    async fn ask(&self, pdu: u8, oids: &[Vec<u32>]) -> anyhow::Result<Vec<(Vec<u32>, Val)>> {
        let id = self.ids.fetch_add(1, Ordering::Relaxed) & 0x7FFF_FFFF;
        let request = encode(id, pdu, &self.community, oids);
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
        socket.connect((self.host.as_str(), PORT)).await?;
        let mut buf = vec![0u8; 8192];
        for _ in 0..TRIES {
            socket.send(&request).await?;
            let answered = tokio::time::timeout(WAIT, async {
                loop {
                    let n = socket.recv(&mut buf).await?;
                    // A late answer to an earlier question: not this one.
                    if let Ok((got, status, binds)) = decode(&buf[..n])
                        && got == id
                    {
                        return anyhow::Ok((status, binds));
                    }
                }
            })
            .await;
            if let Ok(answer) = answered {
                let (status, binds) = answer?;
                if status != 0 {
                    bail!("SNMP: error status {status}");
                }
                return Ok(binds);
            }
        }
        bail!("SNMP: no answer from {}", self.host)
    }

    /// The values of `oids`, in their order.
    ///
    /// # Errors
    /// No answer, or not one per object.
    pub async fn get(&self, oids: &[&[u32]]) -> anyhow::Result<Vec<Val>> {
        let asked: Vec<Vec<u32>> = oids.iter().map(|o| o.to_vec()).collect();
        let got = self.ask(GET, &asked).await?;
        if got.len() != oids.len() {
            bail!("SNMP: {} answers for {} objects", got.len(), oids.len());
        }
        Ok(got.into_iter().map(|(_, v)| v).collect())
    }

    /// A table's rows, read along its `columns` together (one question a
    /// row): each row's index (what follows the column's own number) and a
    /// value per column (`Missing` where a column has no such row).
    ///
    /// # Errors
    /// No answer, or not one per column.
    pub async fn table(&self, columns: &[&[u32]]) -> anyhow::Result<Vec<(Vec<u32>, Vec<Val>)>> {
        let first = columns.first().context("SNMP: no column")?;
        let mut at: Vec<Vec<u32>> = columns.iter().map(|c| c.to_vec()).collect();
        let mut rows = Vec::new();
        while rows.len() < MAX_ROWS {
            let got = self.ask(GET_NEXT, &at).await?;
            if got.len() != columns.len() {
                bail!("SNMP: {} answers for {} columns", got.len(), columns.len());
            }
            let (name, value) = &got[0];
            if *value == Val::Missing || !name.starts_with(first) || name.len() == first.len() {
                break;
            }
            let index = name[first.len()..].to_vec();
            let row = got
                .into_iter()
                .zip(columns)
                .map(|((name, value), column)| {
                    if name.starts_with(column) && name[column.len()..] == index[..] {
                        value
                    } else {
                        Val::Missing
                    }
                })
                .collect();
            rows.push((index.clone(), row));
            at = columns.iter().map(|c| [c, &index[..]].concat()).collect();
        }
        Ok(rows)
    }
}

// ---- what a printer says -------------------------------------------------------------------

const SYS_OBJECT_ID: &[u32] = &[1, 3, 6, 1, 2, 1, 1, 2, 0];
const PAGES: &[u32] = &[1, 3, 6, 1, 2, 1, 43, 10, 2, 1, 4];
const SERIAL: &[u32] = &[1, 3, 6, 1, 2, 1, 43, 5, 1, 1, 17];
const SUPPLY: &[u32] = &[1, 3, 6, 1, 2, 1, 43, 11, 1, 1];
const COLORANT: &[u32] = &[1, 3, 6, 1, 2, 1, 43, 12, 1, 1, 4];
const ALERT: &[u32] = &[1, 3, 6, 1, 2, 1, 43, 18, 1, 1];
const BROTHER: &[u32] = &[1, 3, 6, 1, 4, 1, 2435];
const BROTHER_MAINTENANCE: &[u32] = &[1, 3, 6, 1, 4, 1, 2435, 2, 3, 9, 4, 2, 1, 5, 5, 8, 0];
const BROTHER_FIRMWARE: &[u32] = &[1, 3, 6, 1, 4, 1, 2435, 2, 3, 9, 4, 2, 1, 5, 5, 17, 0];

/// `prtMarkerSuppliesType`: the kinds a driver names.
pub const TONER: i64 = 3;
pub const WASTE_TONER: i64 = 4;
pub const INK: i64 = 5;
pub const INK_CARTRIDGE: i64 = 6;
pub const DRUM: i64 = 9;
pub const FUSER: i64 = 15;

/// One supply: its kind (`prtMarkerSuppliesType`), the printer's words
/// for it, its colour, and what is left in percent (none: unknown, or
/// « some left »).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Supply {
    pub kind: i64,
    pub description: String,
    pub color: Option<String>,
    pub percent: Option<i64>,
}

/// What a printer says over SNMP.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub pages: Option<i64>,
    pub serial: Option<String>,
    pub firmware: Option<String>,
    pub supplies: Vec<Supply>,
    /// The alerts that matter (`prtAlertCode`, severity critical or warning).
    pub alerts: Vec<i64>,
}

/// What is left, in percent, from a level and the most it holds (RFC 3805:
/// −1 other, −2 unknown, −3 « some left »).
fn percent(level: Option<i64>, max: Option<i64>) -> Option<i64> {
    match (level, max) {
        (Some(l), Some(m)) if m > 0 && (0..=m).contains(&l) => Some((l * 100 + m / 2) / m),
        _ => None,
    }
}

/// A Brother maintenance record: 7-byte entries (a code, `01 04`, a value
/// on 4 bytes) up to `FF`. As the `brother` library of Home Assistant reads
/// it (Apache 2.0): `6f` black toner left, `41` drum left, in hundredths
/// of a percent.
fn brother_left(record: &[u8], code: u8) -> Option<i64> {
    record
        .as_chunks::<7>()
        .0
        .iter()
        .take_while(|c| c[0] != 0xFF)
        .find(|c| c[0] == code)
        .map(|c| i64::from(u32::from_be_bytes([c[3], c[4], c[5], c[6]])))
        .map(|v| (v + 50) / 100)
        .filter(|v| (0..=100).contains(v))
}

/// Reads what the printer says. An unanswered printer is an error (it does
/// not speak SNMP, or not to `public`).
///
/// # Errors
/// No answer.
pub async fn read(client: &Client) -> anyhow::Result<Report> {
    let general = client.get(&[SYS_OBJECT_ID]).await?;
    let column = |n: u32| [SUPPLY, &[n]].concat();
    let (desc, kind, colorant, max, level) =
        (column(6), column(5), column(3), column(8), column(9));
    let supplies = client
        .table(&[&desc, &kind, &colorant, &max, &level])
        .await?;
    let colorants = client.table(&[COLORANT]).await.unwrap_or_default();
    let alert = |n: u32| [ALERT, &[n]].concat();
    let (code, severity) = (alert(7), alert(2));
    let alerts = client.table(&[&code, &severity]).await.unwrap_or_default();
    let pages = client.table(&[PAGES]).await.unwrap_or_default();
    let serial = client.table(&[SERIAL]).await.unwrap_or_default();

    let mut report = Report {
        pages: pages.first().and_then(|(_, v)| v[0].int()),
        serial: serial.first().and_then(|(_, v)| v[0].text()),
        ..Report::default()
    };
    for (_, row) in &supplies {
        let color = row[2].int().and_then(|i| {
            colorants
                .iter()
                .find(|(index, _)| index.last().map(|l| i64::from(*l)) == Some(i))
                .and_then(|(_, v)| v[0].text())
        });
        report.supplies.push(Supply {
            kind: row[1].int().unwrap_or(1),
            description: row[0].text().unwrap_or_default(),
            color,
            percent: percent(row[4].int(), row[3].int()),
        });
    }
    // prtAlertSeverityLevel: other 1, critical 3, warning 4, warning that clears itself 5.
    report.alerts = alerts
        .iter()
        .filter(|(_, row)| matches!(row[1].int(), Some(1 | 3..=5)))
        .filter_map(|(_, row)| row[0].int())
        .collect();

    if matches!(&general[0], Val::Oid(o) if o.starts_with(BROTHER))
        && let Ok(v) = client.get(&[BROTHER_MAINTENANCE, BROTHER_FIRMWARE]).await
    {
        report.firmware = v[1].text();
        if let Some(record) = v[0].bytes() {
            brother_fill(&mut report.supplies, record);
        }
    }
    Ok(report)
}

/// A Brother laser's toner and drum, where the standard says only « some left ».
fn brother_fill(supplies: &mut [Supply], record: &[u8]) {
    for s in supplies.iter_mut().filter(|s| s.percent.is_none()) {
        s.percent = match s.kind {
            TONER
                if s.color
                    .as_deref()
                    .is_none_or(|c| c.eq_ignore_ascii_case("black")) =>
            {
                brother_left(record, 0x6F)
            }
            DRUM => brother_left(record, 0x41),
            _ => None,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_what_net_snmp_sends() {
        // `snmpget -v2c -c public host 1.3.6.1.2.1.1.5.0`, request id 1.
        let sent = encode(1, GET, "public", &[vec![1, 3, 6, 1, 2, 1, 1, 5, 0]]);
        assert_eq!(
            sent,
            [
                0x30, 0x26, 0x02, 0x01, 0x01, 0x04, 0x06, b'p', b'u', b'b', b'l', b'i', b'c', 0xA0,
                0x19, 0x02, 0x01, 0x01, 0x02, 0x01, 0x00, 0x02, 0x01, 0x00, 0x30, 0x0E, 0x30, 0x0C,
                0x06, 0x08, 0x2B, 0x06, 0x01, 0x02, 0x01, 0x01, 0x05, 0x00, 0x05, 0x00
            ]
        );
        // Big arcs and integers: base 128, shortest two's complement.
        let mut o = Vec::new();
        oid(&mut o, &[1, 3, 6, 1, 4, 1, 2435]);
        assert_eq!(o, [0x06, 0x07, 0x2B, 0x06, 0x01, 0x04, 0x01, 0x93, 0x03]);
        for (v, bytes) in [
            (0, vec![0]),
            (127, vec![0x7F]),
            (128, vec![0, 0x80]),
            (-3, vec![0xFD]),
        ] {
            let mut i = Vec::new();
            integer(&mut i, v);
            assert_eq!(i[2..], bytes[..], "{v}");
            assert_eq!(signed(&i[2..]), v);
        }
    }

    /// An answer as a printer sends it.
    fn answer(id: i32, binds: &[(&[u32], u8, &[u8])]) -> Vec<u8> {
        let mut list = Vec::new();
        for (name, tag, value) in binds {
            let mut bind = Vec::new();
            oid(&mut bind, name);
            tlv(&mut bind, *tag, value);
            tlv(&mut list, SEQUENCE, &bind);
        }
        let mut body = Vec::new();
        integer(&mut body, i64::from(id));
        integer(&mut body, 0);
        integer(&mut body, 0);
        tlv(&mut body, SEQUENCE, &list);
        let mut message = Vec::new();
        integer(&mut message, V2C);
        tlv(&mut message, OCTETS, b"public");
        tlv(&mut message, RESPONSE, &body);
        let mut out = Vec::new();
        tlv(&mut out, SEQUENCE, &message);
        out
    }

    #[test]
    fn reads_an_answer() {
        let pages: &[u32] = &[1, 3, 6, 1, 2, 1, 43, 10, 2, 1, 4, 1, 1];
        let serial: &[u32] = &[1, 3, 6, 1, 2, 1, 43, 5, 1, 1, 17, 1];
        let missing: &[u32] = &[1, 3, 6, 1, 4, 1, 2435, 9];
        let data = answer(
            42,
            &[
                (pages, COUNTER32, &[0x05, 0x7A]),
                (serial, OCTETS, b"A00000000000000\0"),
                (missing, 0x80, &[]),
                (
                    SYS_OBJECT_ID,
                    OID,
                    &[0x2B, 0x06, 0x01, 0x04, 0x01, 0x93, 0x03, 0x02],
                ),
            ],
        );
        let (id, status, binds) = decode(&data).unwrap();
        assert_eq!((id, status), (42, 0));
        assert_eq!(binds[0], (pages.to_vec(), Val::Int(1402)));
        assert_eq!(binds[1].1.text().as_deref(), Some("A00000000000000"));
        assert_eq!(binds[2].1, Val::Missing);
        assert_eq!(binds[3].1, Val::Oid(vec![1, 3, 6, 1, 4, 1, 2435, 2]));
        // Long content (a Brother record is 50 bytes or more; 300 here).
        let long = answer(1, &[(serial, OCTETS, &[b'x'; 300])]);
        assert_eq!(decode(&long).unwrap().2[0].1.bytes().unwrap().len(), 300);
        assert!(decode(&long[..long.len() - 1]).is_err());
    }

    #[test]
    fn levels_in_percent() {
        // A drum: 8 598 pages left of 10 000.
        assert_eq!(percent(Some(8598), Some(10_000)), Some(86));
        // A toner that only says « some left » (-3), its capacity unknown (-2).
        assert_eq!(percent(Some(-3), Some(-2)), None);
        assert_eq!(percent(Some(0), Some(100)), Some(0));
        assert_eq!(percent(Some(120), Some(100)), None);
    }

    #[test]
    fn a_brother_record_fills_toner_and_drum() {
        // Read from a Brother HL-1210W (October 2026): drum 86 %, toner 8 %.
        let record = [
            0x63, 0x01, 0x04, 0x00, 0x00, 0x00, 0x01, 0x11, 0x01, 0x04, 0x00, 0x00, 0x05, 0x7A,
            0x41, 0x01, 0x04, 0x00, 0x00, 0x21, 0x98, 0x31, 0x01, 0x04, 0x00, 0x00, 0x00, 0x02,
            0x6F, 0x01, 0x04, 0x00, 0x00, 0x03, 0x20, 0x81, 0x01, 0x04, 0x00, 0x00, 0x00, 0x0A,
            0x86, 0x01, 0x04, 0x00, 0x00, 0x00, 0x00, 0xFF,
        ];
        let supply = |kind, percent| Supply {
            kind,
            description: String::new(),
            color: Some("black".into()),
            percent,
        };
        let mut supplies = vec![
            supply(TONER, None),
            supply(DRUM, Some(85)),
            supply(FUSER, None),
        ];
        brother_fill(&mut supplies, &record);
        // The standard's own figure is kept where there is one.
        assert_eq!(
            supplies.iter().map(|s| s.percent).collect::<Vec<_>>(),
            [Some(8), Some(85), None]
        );
        supplies[1].percent = None;
        brother_fill(&mut supplies, &record);
        assert_eq!(supplies[1].percent, Some(86));
    }
}
