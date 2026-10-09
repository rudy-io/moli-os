//! IPP 2.0 messages (RFC 8010), only what a printer driver needs: requests
//! with operation and job attributes (and a document after them), and the
//! attributes of an answer, flattened by name. Collections (`media-col…`)
//! are skipped whole: nothing here reads them.

use std::collections::HashMap;

use anyhow::{Context as _, bail};

pub const GET_PRINTER_ATTRIBUTES: u16 = 0x000B;
pub const PRINT_JOB: u16 = 0x0002;
pub const CANCEL_JOB: u16 = 0x0008;
pub const GET_JOB_ATTRIBUTES: u16 = 0x0009;
pub const IDENTIFY_PRINTER: u16 = 0x003C;

const OPERATION: u8 = 0x01;
const JOB: u8 = 0x02;
const END: u8 = 0x03;

const INTEGER: u8 = 0x21;
const BOOLEAN: u8 = 0x22;
const ENUM: u8 = 0x23;
const BEGIN_COLLECTION: u8 = 0x34;
const END_COLLECTION: u8 = 0x37;
const NAME: u8 = 0x42;
const KEYWORD: u8 = 0x44;
const URI: u8 = 0x45;
const CHARSET: u8 = 0x47;
const LANGUAGE: u8 = 0x48;
const MIME: u8 = 0x49;

/// An attribute value, as far as a driver reads one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    Int(i32),
    Bool(bool),
    Text(String),
    /// Anything else (dates, resolutions, ranges, collections).
    Other,
}

impl Val {
    #[must_use]
    pub fn int(&self) -> Option<i32> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }

    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(t) => Some(t),
            _ => None,
        }
    }
}

/// A request: the operation attributes every request carries (charset,
/// language, printer), then what the caller adds.
pub struct Request {
    op: u16,
    id: u32,
    operation: Vec<u8>,
    job: Vec<u8>,
}

impl Request {
    #[must_use]
    pub fn new(op: u16, id: u32, printer_uri: &str) -> Self {
        let mut r = Self {
            op,
            id,
            operation: Vec::new(),
            job: Vec::new(),
        };
        attr(&mut r.operation, CHARSET, "attributes-charset", b"utf-8");
        attr(
            &mut r.operation,
            LANGUAGE,
            "attributes-natural-language",
            b"fr",
        );
        attr(&mut r.operation, URI, "printer-uri", printer_uri.as_bytes());
        r
    }

    #[must_use]
    pub fn name(mut self, name: &str, value: &str) -> Self {
        attr(&mut self.operation, NAME, name, value.as_bytes());
        self
    }

    #[must_use]
    pub fn mime(mut self, name: &str, value: &str) -> Self {
        attr(&mut self.operation, MIME, name, value.as_bytes());
        self
    }

    #[must_use]
    pub fn integer(mut self, name: &str, value: i32) -> Self {
        attr(&mut self.operation, INTEGER, name, &value.to_be_bytes());
        self
    }

    /// One or several keywords (several: `requested-attributes`).
    #[must_use]
    pub fn keywords(mut self, name: &str, values: &[&str]) -> Self {
        for (i, v) in values.iter().enumerate() {
            attr(
                &mut self.operation,
                KEYWORD,
                if i == 0 { name } else { "" },
                v.as_bytes(),
            );
        }
        self
    }

    #[must_use]
    pub fn job_integer(mut self, name: &str, value: i32) -> Self {
        attr(&mut self.job, INTEGER, name, &value.to_be_bytes());
        self
    }

    #[must_use]
    pub fn job_keyword(mut self, name: &str, value: &str) -> Self {
        attr(&mut self.job, KEYWORD, name, value.as_bytes());
        self
    }

    /// The whole message, the document (if any) after it.
    #[must_use]
    pub fn encode(&self, document: &[u8]) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(64 + self.operation.len() + self.job.len() + document.len());
        out.extend_from_slice(&[2, 0]);
        out.extend_from_slice(&self.op.to_be_bytes());
        out.extend_from_slice(&self.id.to_be_bytes());
        out.push(OPERATION);
        out.extend_from_slice(&self.operation);
        if !self.job.is_empty() {
            out.push(JOB);
            out.extend_from_slice(&self.job);
        }
        out.push(END);
        out.extend_from_slice(document);
        out
    }
}

fn attr(out: &mut Vec<u8>, tag: u8, name: &str, value: &[u8]) {
    out.push(tag);
    out.extend_from_slice(&u16::try_from(name.len()).unwrap_or(u16::MAX).to_be_bytes());
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&u16::try_from(value.len()).unwrap_or(u16::MAX).to_be_bytes());
    out.extend_from_slice(value);
}

/// An answer: its status and every attribute, by name (all groups).
#[derive(Debug, Default)]
pub struct Response {
    pub status: u16,
    pub attrs: HashMap<String, Vec<Val>>,
}

impl Response {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.status < 0x0100
    }

    #[must_use]
    pub fn first(&self, name: &str) -> Option<&Val> {
        self.attrs.get(name).and_then(|v| v.first())
    }

    #[must_use]
    pub fn text(&self, name: &str) -> Option<&str> {
        self.first(name).and_then(Val::text)
    }

    #[must_use]
    pub fn int(&self, name: &str) -> Option<i32> {
        self.first(name).and_then(Val::int)
    }

    #[must_use]
    pub fn texts(&self, name: &str) -> Vec<&str> {
        self.attrs
            .get(name)
            .map(|v| v.iter().filter_map(Val::text).collect())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn ints(&self, name: &str) -> Vec<i32> {
        self.attrs
            .get(name)
            .map(|v| v.iter().filter_map(Val::int).collect())
            .unwrap_or_default()
    }
}

/// Reads an answer.
pub fn parse(data: &[u8]) -> anyhow::Result<Response> {
    if data.len() < 8 {
        bail!("IPP answer of {} bytes", data.len());
    }
    let mut response = Response {
        status: u16::from_be_bytes([data[2], data[3]]),
        attrs: HashMap::new(),
    };
    let mut i = 8;
    let mut last: Option<String> = None;
    // Inside a collection: its members are not attributes of the answer.
    let mut depth = 0u32;
    let take = |i: &mut usize, n: usize| -> anyhow::Result<&[u8]> {
        let bytes = data.get(*i..*i + n).context("IPP answer cut short")?;
        *i += n;
        Ok(bytes)
    };
    while i < data.len() {
        let tag = data[i];
        i += 1;
        if tag == END {
            break;
        }
        if tag < 0x10 {
            continue; // a new group
        }
        let len = take(&mut i, 2)?;
        let name = String::from_utf8_lossy(take(
            &mut i,
            usize::from(u16::from_be_bytes([len[0], len[1]])),
        )?)
        .into_owned();
        let len = take(&mut i, 2)?;
        let raw = take(&mut i, usize::from(u16::from_be_bytes([len[0], len[1]])))?;
        match tag {
            BEGIN_COLLECTION => {
                depth += 1;
                if depth > 1 {
                    continue;
                }
            }
            END_COLLECTION => {
                depth = depth.saturating_sub(1);
                continue;
            }
            _ if depth > 0 => continue,
            _ => {}
        }
        let value = match tag {
            INTEGER | ENUM if raw.len() == 4 => {
                Val::Int(i32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]))
            }
            BOOLEAN if raw.len() == 1 => Val::Bool(raw[0] != 0),
            0x35 | 0x36 | 0x41..=0x49 => Val::Text(String::from_utf8_lossy(raw).into_owned()),
            _ => Val::Other,
        };
        if name.is_empty() {
            // Another value of the attribute before.
            if let Some(name) = &last {
                response.attrs.entry(name.clone()).or_default().push(value);
            }
        } else {
            response.attrs.entry(name.clone()).or_default().push(value);
            last = Some(name);
        }
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MG3600: &[u8] =
        include_bytes!("../../../integrations/ipp/fixtures/mg3600-attributes.ipp");

    #[test]
    fn reads_a_real_printers_attributes() {
        let r = parse(MG3600).unwrap();
        assert!(r.ok());
        assert_eq!(
            r.text("printer-make-and-model"),
            Some("Canon MG3600 series")
        );
        assert_eq!(r.int("printer-state"), Some(3));
        assert_eq!(
            r.texts("printer-state-reasons"),
            ["marker-supply-low-warning"]
        );
        assert_eq!(r.texts("marker-names"), ["Color", "Black"]);
        assert_eq!(r.ints("marker-levels"), [0, 0]);
        assert_eq!(
            r.texts("document-format-supported"),
            [
                "application/octet-stream",
                "image/jpeg",
                "image/urf",
                "image/pwg-raster"
            ]
        );
        assert_eq!(
            r.text("printer-uuid"),
            Some("urn:uuid:00000000-0000-1000-8000-020000000004")
        );
        // A collection's members never leak out as attributes.
        assert!(!r.attrs.contains_key("media-size"));
        assert!(r.attrs.contains_key("media-col-default"));
    }

    #[test]
    fn a_request_reads_back() {
        let message = Request::new(PRINT_JOB, 7, "ipp://printer/ipp/print")
            .name("job-name", "photo.jpg")
            .mime("document-format", "image/jpeg")
            .keywords("requested-attributes", &["printer-state", "marker-levels"])
            .job_integer("copies", 2)
            .job_keyword("sides", "one-sided")
            .encode(b"JPEG");
        assert_eq!(&message[..8], &[2, 0, 0, 2, 0, 0, 0, 7]);
        assert!(message.ends_with(b"\x03JPEG"));
        // The answer parser reads requests too (same encoding).
        let back = parse(&message[..message.len() - 4]).unwrap();
        assert_eq!(back.text("job-name"), Some("photo.jpg"));
        assert_eq!(
            back.texts("requested-attributes"),
            ["printer-state", "marker-levels"]
        );
        assert_eq!(back.int("copies"), Some(2));
        assert_eq!(back.text("sides"), Some("one-sided"));
    }
}
