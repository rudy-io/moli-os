//! Network printers that speak IPP (IPP Everywhere, AirPrint: most printers
//! since 2013): their state in words, each cartridge's level, the queue, and
//! printing from Moli (a photo, the pages of a PDF the dashboard rendered).
//! An older printer may speak IPP 1.1 only and say its supplies only over
//! SNMP (`snmp.rs`): pages printed, toner and drum, alerts.
//!
//! Printing goes through [`moli_runtime::media::PrintSink`]: the document is
//! queued at once, a worker sends it (an inkjet takes it as it prints: a
//! minute or more), and its progress shows in the `job` point.

mod proto;
mod snmp;

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, bail};
use http::Method;
use http_body_util::Full;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::media::{Document, PrintSink};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use proto::{Request, Response};
use serde::Deserialize;
use tokio::sync::mpsc;

/// The state, when nothing prints.
const POLL: Duration = Duration::from_secs(30);
/// While a job prints: its progress.
const POLL_BUSY: Duration = Duration::from_secs(4);
/// Supplies, pages and alerts over SNMP: they change slowly.
const SNMP_EVERY: Duration = Duration::from_secs(5 * 60);
/// SNMP unanswered this many times in a row while IPP answers: the printer
/// does not speak it (asked no more until Moli restarts).
const SNMP_MISSES: u8 = 3;
/// A question to the printer.
const ASK: Duration = Duration::from_secs(8);
/// Sending a document: the printer reads it as it prints (and stops reading
/// while a person puts paper back).
const SEND: Duration = Duration::from_secs(15 * 60);
const MAX_ANSWER: usize = 1024 * 1024;
/// Documents waiting to be sent (the pages of a long PDF).
const QUEUE: usize = 64;
/// The queue's weight at most: Moli lives in 128 MB (a page from the
/// dashboard weighs 0.5 to 3 MB; the dashboard retries when it is full).
const MAX_QUEUED_BYTES: usize = 32 * 1024 * 1024;
/// Said when the queue is full (the dashboard waits and tries again on it):
/// the words themselves are `pilotes.ipp.file_pleine` in the catalogue.
#[cfg(test)]
const QUEUE_FULL: &str = "file d'impression pleine";
const MAX_COPIES: u32 = 20;
const USER: &str = "Moli";

const STATUS_ATTRS: &[&str] = &[
    "printer-state",
    "printer-state-reasons",
    "printer-make-and-model",
    "printer-info",
    "marker-names",
    "marker-levels",
    "queued-job-count",
    "printer-is-accepting-jobs",
    "document-format-supported",
    "sides-supported",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The printer's address on the network.
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Where it answers IPP (`/ipp/print` for IPP Everywhere; `/ipp` on
    /// older Brother printers).
    #[serde(default = "default_path")]
    pub path: String,
    /// Also read it over SNMP (`public`): pages, toner, drum, alerts.
    #[serde(default = "default_snmp")]
    pub snmp: bool,
}

fn default_snmp() -> bool {
    true
}

fn default_port() -> u16 {
    631
}

fn default_path() -> String {
    "/ipp/print".into()
}

#[derive(Debug)]
pub struct Ipp {
    config: Config,
}

impl Ipp {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Ipp {
    fn kind(&self) -> &'static str {
        "ipp"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

// ---- talking to the printer ---------------------------------------------------------

#[derive(Debug)]
struct Printer {
    host: String,
    port: u16,
    path: String,
    uri: String,
    ids: AtomicU32,
    /// It refused IPP 2.0: spoken to in 1.1 from then on.
    legacy: AtomicBool,
}

impl Printer {
    fn new(config: &Config) -> Self {
        Self {
            host: config.host.clone(),
            port: config.port,
            path: config.path.clone(),
            uri: format!("ipp://{}:{}{}", config.host, config.port, config.path),
            ids: AtomicU32::new(1),
            legacy: AtomicBool::new(false),
        }
    }

    fn request(&self, op: u16) -> Request {
        let request = Request::new(op, self.ids.fetch_add(1, Ordering::Relaxed), &self.uri);
        if self.legacy.load(Ordering::Relaxed) {
            request.v1_1()
        } else {
            request
        }
    }

    async fn call(
        &self,
        request: &Request,
        document: &[u8],
        limit: Duration,
    ) -> anyhow::Result<Response> {
        checked(self.send(request, document, limit).await?)
    }

    /// One request and its answer, whatever its status.
    async fn send(
        &self,
        request: &Request,
        document: &[u8],
        limit: Duration,
    ) -> anyhow::Result<Response> {
        let body = request.encode(document);
        let http = http::Request::builder()
            .method(Method::POST)
            .uri(&self.path)
            .header("content-type", "application/ipp")
            .body(Full::new(moli_net::Body::from(body)))?;
        let (status, bytes) =
            moli_net::web(&self.host, self.port, false, http, limit, MAX_ANSWER).await?;
        if !status.is_success() {
            bail!(moli_i18n::tr!("pilotes.ipp.http", status = status));
        }
        proto::parse(&bytes)
    }

    /// The printer's state; a printer that refuses IPP 2.0 is asked again
    /// in 1.1, and spoken to in 1.1 from then on.
    async fn status(&self) -> anyhow::Result<Response> {
        let ask = || {
            self.request(proto::GET_PRINTER_ATTRIBUTES)
                .keywords("requested-attributes", STATUS_ATTRS)
        };
        let answer = self.send(&ask(), &[], ASK).await?;
        if answer.status == proto::VERSION_NOT_SUPPORTED
            && !self.legacy.swap(true, Ordering::Relaxed)
        {
            tracing::info!(host = %self.host, "printer speaks IPP 1.1, not 2.0");
            return self.call(&ask(), &[], ASK).await;
        }
        checked(answer)
    }

    /// Sends one document; the printer's job id.
    async fn print(&self, doc: &Document, sides: bool) -> anyhow::Result<i32> {
        let mut request = self
            .request(proto::PRINT_JOB)
            .name("requesting-user-name", USER)
            .name("job-name", &doc.name)
            .mime("document-format", &doc.content_type)
            .job_keyword(
                "print-color-mode",
                if doc.color { "color" } else { "monochrome" },
            );
        if doc.copies > 1 {
            request = request.job_integer("copies", i32::try_from(doc.copies).unwrap_or(1));
        }
        if sides {
            request = request.job_keyword(
                "sides",
                if doc.two_sided {
                    "two-sided-long-edge"
                } else {
                    "one-sided"
                },
            );
        }
        let answer = self.call(&request, &doc.bytes, SEND).await?;
        answer
            .int("job-id")
            .with_context(|| moli_i18n::tr!("pilotes.ipp.sans_numero"))
    }

    async fn job(&self, id: i32) -> anyhow::Result<Response> {
        let request = self
            .request(proto::GET_JOB_ATTRIBUTES)
            .integer("job-id", id)
            .keywords("requested-attributes", &["job-state", "job-state-reasons"]);
        self.call(&request, &[], ASK).await
    }

    async fn cancel(&self, id: i32) -> anyhow::Result<()> {
        let request = self
            .request(proto::CANCEL_JOB)
            .integer("job-id", id)
            .name("requesting-user-name", USER);
        self.call(&request, &[], ASK).await.map(|_| ())
    }

    async fn identify(&self) -> anyhow::Result<()> {
        let request = self
            .request(proto::IDENTIFY_PRINTER)
            .keywords("identify-actions", &["flash"])
            .name("requesting-user-name", USER);
        self.call(&request, &[], ASK).await.map(|_| ())
    }
}

/// An answer the printer refused, said as an error.
fn checked(answer: Response) -> anyhow::Result<Response> {
    if !answer.ok() {
        let said = answer
            .text("status-message")
            .map(|m| format!(" ({m})"))
            .unwrap_or_default();
        bail!(moli_i18n::tr!(
            "pilotes.ipp.refuse",
            code = format!("{:#06x}", answer.status),
            said = said
        ));
    }
    Ok(answer)
}

// ---- what the printer says, in words ----------------------------------------------------

/// The printer's state as one sentence, the most pressing first.
fn status_words(state: Option<i32>, reasons: &[&str]) -> String {
    let has = |r: &str| reasons.iter().any(|x| x.starts_with(r));
    moli_i18n::tr(if has("media-jam") {
        "pilotes.ipp.bourrage"
    } else if has("media-empty") || has("media-needed") {
        "pilotes.ipp.plus_de_papier"
    } else if has("door-open") || has("cover-open") {
        "pilotes.ipp.capot"
    } else if has("marker-supply-empty") || has("marker-supply-missing") {
        "pilotes.ipp.cartouche"
    } else if has("paused") {
        "pilotes.ipp.pause"
    } else if state == Some(4) {
        "pilotes.ipp.impression"
    } else if state == Some(5) {
        "pilotes.ipp.arretee"
    } else if has("marker-supply-low") {
        "pilotes.ipp.encre_bientot_vide"
    } else if has("toner-low") {
        "pilotes.ipp.toner_bientot_vide"
    } else if has("drum-end") {
        "pilotes.ipp.tambour_a_changer"
    } else if has("drum-near-end") {
        "pilotes.ipp.tambour_bientot"
    } else {
        "pilotes.ipp.prete"
    })
}

/// An SNMP alert (`prtAlertCode`, RFC 3805) as the state reason IPP would
/// give, or one of Moli's own for a laser (`toner-low`, `drum-…`).
fn alert_reason(code: i64) -> Option<&'static str> {
    Some(match code {
        3 | 5 | 501 => "cover-open",
        8 => "media-jam",
        808 => "media-empty",
        1101 | 1102 | 1115 => "marker-supply-empty",
        1104 => "toner-low",
        1105 => "marker-supply-low",
        1111 => "drum-near-end",
        1112 => "drum-end",
        _ => return None,
    })
}

fn state_word(state: Option<i32>) -> Option<&'static str> {
    match state? {
        3 => Some("idle"),
        4 => Some("processing"),
        5 => Some("stopped"),
        _ => None,
    }
}

/// « Color » → « couleur »: a cartridge as the family names it.
fn ink_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let known = match lower.as_str() {
        "color" | "colour" | "tri-color" | "tricolor" => "pilotes.ipp.encre_couleur",
        "black" => "pilotes.ipp.encre_noire",
        "photo black" => "pilotes.ipp.encre_noire_photo",
        "yellow" => "pilotes.ipp.encre_jaune",
        "cyan" => "pilotes.ipp.encre_cyan",
        "magenta" => "pilotes.ipp.encre_magenta",
        _ => return moli_i18n::tr!("pilotes.ipp.encre", name = name),
    };
    moli_i18n::tr(known)
}

/// A supply read over SNMP, as the family names it.
fn supply_name(s: &snmp::Supply) -> String {
    let color = s.color.as_deref().unwrap_or_default().to_lowercase();
    let key = match s.kind {
        snmp::TONER => match color.as_str() {
            "black" => "pilotes.ipp.toner_noir",
            "cyan" => "pilotes.ipp.toner_cyan",
            "magenta" => "pilotes.ipp.toner_magenta",
            "yellow" => "pilotes.ipp.toner_jaune",
            _ => "pilotes.ipp.toner",
        },
        snmp::DRUM => "pilotes.ipp.tambour",
        snmp::WASTE_TONER => "pilotes.ipp.toner_usage",
        snmp::FUSER => "pilotes.ipp.unite_fusion",
        snmp::INK | snmp::INK_CARTRIDGE if !color.is_empty() => return ink_name(&color),
        _ if !s.description.is_empty() => return s.description.clone(),
        _ => "pilotes.ipp.consommable",
    };
    moli_i18n::tr(key)
}

/// A level in percent; the negative codes mean « unknown ».
fn level(v: i32) -> Value {
    if (0..=100).contains(&v) {
        Value::Int(i64::from(v))
    } else {
        Value::Null
    }
}

fn job_words(state: i32, reasons: &[&str]) -> String {
    moli_i18n::tr(match state {
        3 | 4 => "pilotes.ipp.job_attente",
        6 if reasons.iter().any(|r| r.starts_with("printer-stopped")) => "pilotes.ipp.job_bloquee",
        6 => "pilotes.ipp.job_interrompue",
        7 => "pilotes.ipp.job_annulee",
        8 => "pilotes.ipp.job_echouee",
        9 => "pilotes.ipp.job_terminee",
        _ => "pilotes.ipp.job_en_cours",
    })
}

// ---- the device ------------------------------------------------------------------------------

fn spec(
    key: &str,
    label: &str,
    kind: Kind,
    write: bool,
    semantic: Semantic,
    unit: Option<Unit>,
) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        semantic,
        kind,
        access: Access { read: true, write },
        unit,
    }
}

/// The points a printer has beyond IPP's, from what it says over SNMP.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct More {
    pages: bool,
    serial: bool,
    firmware: bool,
}

impl More {
    fn of(report: Option<&snmp::Report>) -> Self {
        report.map_or_else(Self::default, |r| Self {
            pages: r.pages.is_some(),
            serial: r.serial.is_some(),
            firmware: r.firmware.is_some(),
        })
    }
}

/// The points SNMP adds: pages printed, serial number, firmware.
fn more_points(more: More) -> Vec<PointSpec> {
    let text = |key: &str, label: &str| spec(key, label, Kind::Text, false, Semantic::Other, None);
    let mut points = Vec::new();
    if more.pages {
        points.push(spec(
            "pages",
            &moli_i18n::tr!("pilotes.ipp.pages"),
            Kind::Numeric {
                min: Some(0.0),
                max: None,
                step: Some(1.0),
            },
            false,
            Semantic::Other,
            None,
        ));
    }
    if more.serial {
        points.push(text("serial", &moli_i18n::tr!("pilotes.ipp.numero_serie")));
    }
    if more.firmware {
        points.push(text(
            "firmware",
            &moli_i18n::tr!("pilotes.ipp.micrologiciel"),
        ));
    }
    points
}

/// The device; `inks`: each cartridge or supply's name, as shown.
fn device(
    ctx: &DriverCtx,
    id: &DeviceId,
    model: Option<&str>,
    inks: &[String],
    more: More,
) -> Device {
    let (manufacturer, model) = match model.and_then(|m| m.split_once(' ')) {
        Some((brand, rest)) => (Some(brand.into()), Some(rest.into())),
        None => (None, model.map(Into::into)),
    };
    let percent = Kind::Numeric {
        min: Some(0.0),
        max: Some(100.0),
        step: None,
    };
    let mut points = vec![
        spec(
            "state",
            &moli_i18n::tr!("pilotes.ipp.etat"),
            Kind::Enum {
                values: vec!["idle".into(), "processing".into(), "stopped".into()],
            },
            false,
            Semantic::Other,
            None,
        ),
        spec(
            "status",
            &moli_i18n::tr!("pilotes.ipp.message"),
            Kind::Text,
            false,
            Semantic::Other,
            None,
        ),
        spec(
            "ink_low",
            &moli_i18n::tr!("pilotes.ipp.encre_basse"),
            Kind::Binary,
            false,
            Semantic::Other,
            None,
        ),
        spec(
            "jobs",
            &moli_i18n::tr!("pilotes.ipp.impressions_en_attente"),
            Kind::Numeric {
                min: Some(0.0),
                max: None,
                step: Some(1.0),
            },
            false,
            Semantic::Other,
            None,
        ),
        spec(
            "job",
            &moli_i18n::tr!("pilotes.ipp.derniere_impression"),
            Kind::Text,
            false,
            Semantic::Other,
            None,
        ),
        spec(
            "identify",
            &moli_i18n::tr!("pilotes.ipp.clignoter"),
            Kind::Binary,
            true,
            Semantic::Other,
            None,
        ),
        // An agent asking to cancel a print gets a human's approval first.
        spec(
            "cancel",
            &moli_i18n::tr!("pilotes.ipp.annuler"),
            Kind::Binary,
            true,
            Semantic::Control,
            None,
        ),
    ];
    points.extend(more_points(more));
    for (i, name) in inks.iter().enumerate() {
        points.push(spec(
            &format!("ink_{}", i + 1),
            name,
            percent.clone(),
            false,
            Semantic::Other,
            Some(Unit::Percent),
        ));
    }
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.ipp.imprimante").into(),
        manufacturer,
        model,
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

// ---- printing --------------------------------------------------------------------------------

/// What the sink, the worker and the run loop share about printing.
#[derive(Debug, Default)]
struct Spool {
    online: AtomicBool,
    two_sided: AtomicBool,
    /// Out of paper, jammed, cover open: the next page waits.
    stopped: AtomicBool,
    /// Bumped by « cancel »: whatever was queued before it is dropped.
    epoch: AtomicU64,
    /// What waits in the queue (Moli lives in 128 MB: bounded in bytes).
    bytes: AtomicUsize,
    waiting: AtomicUsize,
    /// What the printer said it takes (empty: not known yet).
    formats: Mutex<Vec<String>>,
}

/// A document in the queue, with the « cancel » epoch it was queued in.
struct Queued {
    doc: Document,
    epoch: u64,
}

/// What the dashboard hands over: checked, then queued for the worker.
#[derive(Debug)]
struct Sink {
    queue: mpsc::Sender<Queued>,
    spool: Arc<Spool>,
}

impl PrintSink for Sink {
    fn print(&self, document: Document) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move {
            let spool = &self.spool;
            if !spool.online.load(Ordering::Relaxed) {
                bail!(moli_i18n::tr!("pilotes.ipp.eteinte"));
            }
            let formats = spool
                .formats
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if !formats.is_empty() && !formats.contains(&document.content_type) {
                bail!(moli_i18n::tr!(
                    "pilotes.ipp.format_refuse",
                    format = document.content_type,
                    accepted = formats.join(", ")
                ));
            }
            if document.copies == 0 || document.copies > MAX_COPIES {
                bail!(moli_i18n::tr!("pilotes.ipp.exemplaires", max = MAX_COPIES));
            }
            if document.bytes.is_empty() {
                bail!(moli_i18n::tr!("pilotes.ipp.document_vide"));
            }
            let size = document.bytes.len();
            if spool.bytes.load(Ordering::Relaxed) + size > MAX_QUEUED_BYTES {
                bail!(moli_i18n::tr!("pilotes.ipp.file_pleine"));
            }
            let epoch = spool.epoch.load(Ordering::Relaxed);
            self.queue
                .try_send(Queued {
                    doc: document,
                    epoch,
                })
                .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("pilotes.ipp.file_pleine")))?;
            spool.bytes.fetch_add(size, Ordering::Relaxed);
            spool.waiting.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
    }
}

/// What the worker tells the run loop.
enum Sent {
    /// Sending: the name, how many wait behind it.
    Sending(String, usize),
    /// Taken by the printer: its job id.
    Taken(String, i32),
    Failed(String, String),
    /// Cancelled while it was being sent (the printer's job is cancelled).
    Dropped(String),
}

/// Sends the queued documents one after the other.
fn worker(
    printer: Arc<Printer>,
    spool: Arc<Spool>,
    mut queue: mpsc::Receiver<Queued>,
    news: mpsc::Sender<Sent>,
) {
    tokio::spawn(async move {
        while let Some(Queued { doc, epoch }) = queue.recv().await {
            spool.bytes.fetch_sub(doc.bytes.len(), Ordering::Relaxed);
            spool.waiting.fetch_sub(1, Ordering::Relaxed);
            let cancelled = || spool.epoch.load(Ordering::Relaxed) != epoch;
            // Out of paper: wait for a person (or a cancel).
            while spool.stopped.load(Ordering::Relaxed) && !cancelled() {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            if cancelled() {
                continue;
            }
            let behind = spool.waiting.load(Ordering::Relaxed);
            let _ = news.send(Sent::Sending(doc.name.clone(), behind)).await;
            tracing::info!(document = ?doc, "printing");
            let sent = match printer
                .print(&doc, spool.two_sided.load(Ordering::Relaxed))
                .await
            {
                Ok(job) if cancelled() => {
                    let _ = printer.cancel(job).await;
                    Sent::Dropped(doc.name)
                }
                Ok(job) => Sent::Taken(doc.name, job),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), name = %doc.name, "print failed");
                    Sent::Failed(doc.name, format!("{e:#}"))
                }
            };
            if news.send(sent).await.is_err() {
                return;
            }
        }
    });
}

// ---- the run loop ------------------------------------------------------------------------------

/// The state worth keeping between polls.
#[derive(Default)]
struct Seen {
    model: Option<String>,
    /// The cartridges or supplies, as named on the device.
    inks: Vec<String>,
    more: More,
    online: Option<bool>,
    /// SNMP: who to ask (none: not asked), when next, unanswered in a
    /// row, and what it said last.
    snmp: Option<snmp::Client>,
    snmp_next: Option<tokio::time::Instant>,
    snmp_misses: u8,
    report: Option<snmp::Report>,
}

/// What prints now: the page being sent, the job the printer has.
#[derive(Default)]
struct Printing {
    sending: Option<String>,
    current: Option<(i32, String)>,
}

impl Printing {
    fn busy(&self, spool: &Spool) -> bool {
        self.sending.is_some()
            || self.current.is_some()
            || spool.waiting.load(Ordering::Relaxed) > 0
    }
}

/// « Cancel »: the queue and the page being sent are dropped, the job the
/// printer has is cancelled.
async fn cancel(printer: &Printer, spool: &Spool, printing: &mut Printing) -> Result<(), String> {
    if !printing.busy(spool) {
        return Err(moli_i18n::tr!("pilotes.ipp.rien_en_cours"));
    }
    spool.epoch.fetch_add(1, Ordering::Relaxed);
    if let Some((job, _)) = printing.current.take() {
        // Already printed: nothing to say about it.
        let _ = printer.cancel(job).await;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // the run loop, one arm per source of news
async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let printer = Arc::new(Printer::new(config));
    let id = ctx.device_id(&config.host);
    ctx.upsert_device(device(ctx, &id, None, &[], More::default()));
    let spool = Arc::new(Spool::default());
    let (queue, queued) = mpsc::channel(QUEUE);
    let (news_tx, mut news) = mpsc::channel(16);
    worker(Arc::clone(&printer), Arc::clone(&spool), queued, news_tx);
    ctx.provide_printing(
        &id,
        Arc::new(Sink {
            queue,
            spool: Arc::clone(&spool),
        }),
    );
    ctx.ready();

    let mut seen = Seen {
        snmp: config
            .snmp
            .then(|| snmp::Client::new(&config.host, "public")),
        ..Seen::default()
    };
    let mut printing = Printing::default();
    let mut next = tokio::time::Instant::now();
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                let result = match (&*command.key, &command.value) {
                    ("identify", Value::Bool(true)) => printer.identify().await.map_err(|e| format!("{e:#}")),
                    ("cancel", Value::Bool(true)) => {
                        let done = cancel(&printer, &spool, &mut printing).await;
                        if done.is_ok() {
                            ctx.set_state(
                                &id,
                                "job",
                                Value::Text(moli_i18n::tr!("pilotes.ipp.impression_annulee").into()),
                            );
                            next = tokio::time::Instant::now();
                        }
                        done
                    }
                    // Buttons: releasing them does nothing.
                    ("identify" | "cancel", Value::Bool(false)) => Ok(()),
                    (key, _) => Err(moli_i18n::tr!("pilotes.ipp.lecture_seule", point = key)),
                };
                command.reply(result);
            }
            Some(sent) = news.recv() => {
                let line = match sent {
                    Sent::Sending(name, behind) => {
                        let line = if behind > 0 {
                            moli_i18n::tr!("pilotes.ipp.envoi_encore", name = name, behind = behind)
                        } else {
                            moli_i18n::tr!("pilotes.ipp.envoi", name = name)
                        };
                        printing.sending = Some(name);
                        line
                    }
                    Sent::Taken(name, job) => {
                        printing.sending = None;
                        printing.current = Some((job, name.clone()));
                        next = tokio::time::Instant::now() + POLL_BUSY;
                        moli_i18n::tr!(
                            "pilotes.ipp.job_ligne",
                            name = name,
                            state = job_words(3, &[])
                        )
                    }
                    Sent::Failed(name, error) => {
                        printing.sending = None;
                        moli_i18n::tr!("pilotes.ipp.job_echec", name = name, error = error)
                    }
                    Sent::Dropped(name) => {
                        printing.sending = None;
                        moli_i18n::tr!(
                            "pilotes.ipp.job_ligne",
                            name = name,
                            state = job_words(7, &[])
                        )
                    }
                };
                ctx.set_state(&id, "job", Value::Text(line.into()));
            }
            () = tokio::time::sleep_until(next) => {
                poll(&printer, ctx, &id, &mut seen, &spool).await;
                if let Some((job, name)) = printing.current.clone() {
                    match printer.job(job).await {
                        Ok(answer) => {
                            let state = answer.int("job-state").unwrap_or(5);
                            // While the next page is being sent, that is the news.
                            if printing.sending.is_none() {
                                let words = job_words(state, &answer.texts("job-state-reasons"));
                                ctx.set_state(
                                    &id,
                                    "job",
                                    Value::Text(
                                        moli_i18n::tr!(
                                            "pilotes.ipp.job_ligne",
                                            name = name,
                                            state = words
                                        )
                                        .into(),
                                    ),
                                );
                            }
                            if state >= 7 {
                                printing.current = None;
                                // Its pages are counted now.
                                seen.snmp_next = None;
                            }
                        }
                        // Gone from the printer's memory: it printed long ago.
                        Err(_) if seen.online == Some(true) => printing.current = None,
                        Err(_) => {}
                    }
                }
                next = tokio::time::Instant::now() + if printing.busy(&spool) { POLL_BUSY } else { POLL };
            }
        }
    }
}

async fn poll(printer: &Printer, ctx: &DriverCtx, id: &DeviceId, seen: &mut Seen, spool: &Spool) {
    let answer = match printer.status().await {
        Ok(answer) => answer,
        Err(e) => {
            if seen.online != Some(false) {
                seen.online = Some(false);
                spool.online.store(false, Ordering::Relaxed);
                ctx.set_availability(id, false);
                // Switched off at the button: the usual case, not a fault.
                tracing::info!(instance = %ctx.instance(), error = %format!("{e:#}"), "printer unreachable (switched off?)");
            }
            return;
        }
    };
    if seen.online != Some(true) {
        seen.online = Some(true);
        spool.online.store(true, Ordering::Relaxed);
        ctx.set_availability(id, true);
        // Back on: what it says over SNMP is read at once.
        seen.snmp_next = None;
    }
    read_snmp(ctx, seen).await;

    let model = answer
        .text("printer-make-and-model")
        .or_else(|| answer.text("printer-info"))
        .map(str::to_owned);
    // The cartridges IPP names; else the supplies SNMP says.
    let names = answer.texts("marker-names");
    let (inks, levels): (Vec<String>, Vec<Value>) = if names.is_empty() {
        let supplies = seen.report.as_ref().map_or(&[][..], |r| &r.supplies[..]);
        (
            supplies.iter().map(supply_name).collect(),
            supplies
                .iter()
                .map(|s| s.percent.map_or(Value::Null, Value::Int))
                .collect(),
        )
    } else {
        (
            names.iter().map(|n| ink_name(n)).collect(),
            answer
                .ints("marker-levels")
                .into_iter()
                .map(level)
                .collect(),
        )
    };
    let more = More::of(seen.report.as_ref());
    if model != seen.model || inks != seen.inks || more != seen.more {
        ctx.upsert_device(device(ctx, id, model.as_deref(), &inks, more));
        seen.model = model;
        seen.inks = inks;
        seen.more = more;
    }
    *spool.formats.lock().unwrap_or_else(PoisonError::into_inner) = answer
        .texts("document-format-supported")
        .into_iter()
        .map(str::to_owned)
        .collect();
    spool.two_sided.store(
        answer
            .texts("sides-supported")
            .iter()
            .any(|s| s.starts_with("two-sided")),
        Ordering::Relaxed,
    );

    let state = answer.int("printer-state");
    let mut reasons = answer.texts("printer-state-reasons");
    if let Some(report) = &seen.report {
        reasons.extend(report.alerts.iter().filter_map(|&code| alert_reason(code)));
    }
    spool
        .stopped
        .store(waits_for_a_person(state, &reasons), Ordering::Relaxed);
    if let Some(word) = state_word(state) {
        ctx.set_state(id, "state", Value::Text(word.into()));
    }
    ctx.set_state(
        id,
        "status",
        Value::Text(status_words(state, &reasons).into()),
    );
    let low = reasons.iter().any(|r| {
        r.starts_with("marker-supply-low")
            || r.starts_with("marker-supply-empty")
            || *r == "toner-low"
    });
    ctx.set_state(id, "ink_low", Value::Bool(low));
    for (i, v) in levels.into_iter().enumerate() {
        ctx.set_state(id, &format!("ink_{}", i + 1), v);
    }
    if let Some(n) = answer.int("queued-job-count") {
        ctx.set_state(id, "jobs", Value::Int(i64::from(n)));
    }
    if let Some(report) = &seen.report {
        show_report(ctx, id, report);
    }
}

/// What SNMP says beyond the supplies: pages, serial number, firmware.
fn show_report(ctx: &DriverCtx, id: &DeviceId, report: &snmp::Report) {
    if let Some(pages) = report.pages {
        ctx.set_state(id, "pages", Value::Int(pages));
    }
    if let Some(serial) = &report.serial {
        ctx.set_state(id, "serial", Value::Text(serial.as_str().into()));
    }
    if let Some(firmware) = &report.firmware {
        ctx.set_state(id, "firmware", Value::Text(firmware.as_str().into()));
    }
}

/// Asks SNMP when it is time: every `SNMP_EVERY`, at once when the printer
/// comes back or a print ends; given up after `SNMP_MISSES` in a row.
async fn read_snmp(ctx: &DriverCtx, seen: &mut Seen) {
    let now = tokio::time::Instant::now();
    let Some(client) = seen
        .snmp
        .as_ref()
        .filter(|_| seen.snmp_next.is_none_or(|at| now >= at))
    else {
        return;
    };
    match snmp::read(client).await {
        Ok(report) => {
            seen.snmp_misses = 0;
            seen.report = Some(report);
            seen.snmp_next = Some(now + SNMP_EVERY);
        }
        Err(e) => {
            seen.snmp_misses += 1;
            if seen.snmp_misses >= SNMP_MISSES {
                tracing::info!(instance = %ctx.instance(), error = %format!("{e:#}"), "printer does not answer SNMP: its supplies from IPP only");
                seen.snmp = None;
            } else {
                seen.snmp_next = Some(now + POLL);
            }
        }
    }
}

/// Stopped on something only a person fixes: the next page waits for it.
fn waits_for_a_person(state: Option<i32>, reasons: &[&str]) -> bool {
    let has = |r: &str| reasons.iter().any(|x| x.starts_with(r));
    state == Some(5)
        || has("media-empty")
        || has("media-needed")
        || has("media-jam")
        || has("door-open")
        || has("cover-open")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_the_printers_state_in_words() {
        assert_eq!(status_words(Some(3), &["none"]), "Prête");
        assert_eq!(
            status_words(Some(3), &["marker-supply-low-warning"]),
            "Prête, encre bientôt vide"
        );
        assert_eq!(
            status_words(Some(5), &["media-empty-error"]),
            "Plus de papier"
        );
        assert_eq!(
            status_words(Some(4), &["marker-supply-low-warning"]),
            "Impression en cours"
        );
        assert_eq!(
            status_words(Some(5), &["media-jam-error", "door-open"]),
            "Bourrage papier"
        );
    }

    #[test]
    fn snmp_alerts_say_what_ipp_does_not() {
        // A Brother HL-1210W: IPP says « none », SNMP alert 1104 (toner almost empty).
        let reasons: Vec<&str> = [1104, 0].into_iter().filter_map(alert_reason).collect();
        assert_eq!(status_words(Some(3), &reasons), "Prête, toner bientôt vide");
        assert_eq!(
            status_words(Some(3), &[alert_reason(808).unwrap()]),
            "Plus de papier"
        );
        assert!(waits_for_a_person(Some(3), &[alert_reason(8).unwrap()]));
        assert_eq!(
            status_words(Some(3), &[alert_reason(1111).unwrap()]),
            "Prête, tambour bientôt à changer"
        );
    }

    #[test]
    fn names_the_supplies_snmp_says() {
        let supply = |kind, color: Option<&str>, description: &str| snmp::Supply {
            kind,
            description: description.into(),
            color: color.map(Into::into),
            percent: None,
        };
        assert_eq!(
            supply_name(&supply(snmp::TONER, Some("black"), "Black Toner Cartridge")),
            "Toner noir"
        );
        assert_eq!(
            supply_name(&supply(snmp::DRUM, None, "Drum Unit")),
            "Tambour"
        );
        assert_eq!(
            supply_name(&supply(snmp::INK_CARTRIDGE, Some("cyan"), "C")),
            "Encre cyan"
        );
        assert_eq!(supply_name(&supply(1, None, "Belt Unit")), "Belt Unit");
    }

    #[test]
    fn names_the_cartridges_and_their_levels() {
        assert_eq!(ink_name("Color"), "Encre couleur");
        assert_eq!(ink_name("Black"), "Encre noire");
        assert_eq!(ink_name("Light Gray"), "Encre Light Gray");
        assert_eq!(level(42), Value::Int(42));
        assert_eq!(level(0), Value::Int(0));
        // -1 other, -2 unknown, -3 « some left »: not a number to show.
        assert_eq!(level(-3), Value::Null);
    }

    #[test]
    fn jobs_in_words() {
        assert_eq!(job_words(9, &[]), "terminée");
        assert_eq!(job_words(5, &[]), "en cours");
        assert_eq!(
            job_words(6, &["printer-stopped"]),
            "bloquée (voir l'imprimante)"
        );
        assert_eq!(job_words(7, &[]), "annulée");
    }

    #[tokio::test]
    async fn the_sink_refuses_what_the_printer_cannot_take() {
        let (queue, mut queued) = mpsc::channel(4);
        let spool = Arc::new(Spool::default());
        spool.online.store(true, Ordering::Relaxed);
        *spool.formats.lock().unwrap() = vec!["image/jpeg".into()];
        let sink = Sink {
            queue,
            spool: Arc::clone(&spool),
        };
        let doc = |content_type: &str, copies, size| Document {
            name: "photo.jpg".into(),
            content_type: content_type.into(),
            bytes: vec![1; size],
            copies,
            color: true,
            two_sided: false,
        };
        assert!(sink.print(doc("application/pdf", 1, 3)).await.is_err());
        assert!(sink.print(doc("image/jpeg", 0, 3)).await.is_err());
        sink.print(doc("image/jpeg", 2, 3)).await.unwrap();
        assert_eq!(spool.waiting.load(Ordering::Relaxed), 1);
        let q = queued.recv().await.unwrap();
        assert_eq!((q.doc.copies, q.epoch), (2, 0));
        // Bounded in bytes, said so (the dashboard waits on it).
        let too_much = sink
            .print(doc("image/jpeg", 1, MAX_QUEUED_BYTES))
            .await
            .unwrap_err();
        assert!(too_much.to_string().contains(QUEUE_FULL));
        spool.online.store(false, Ordering::Relaxed);
        assert!(sink.print(doc("image/jpeg", 1, 3)).await.is_err());
    }

    #[tokio::test]
    async fn cancel_drops_the_queue_and_needs_something_to_cancel() {
        let printer = Printer::new(&Config {
            host: "192.0.2.1".into(),
            port: 631,
            path: "/ipp/print".into(),
            snmp: false,
        });
        let spool = Spool::default();
        let mut printing = Printing::default();
        assert!(cancel(&printer, &spool, &mut printing).await.is_err());
        spool.waiting.store(3, Ordering::Relaxed);
        cancel(&printer, &spool, &mut printing).await.unwrap();
        // Every page queued before is now of an old epoch: the worker drops it.
        assert_eq!(spool.epoch.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn paper_out_holds_the_queue() {
        assert!(waits_for_a_person(Some(5), &["media-empty-error"]));
        assert!(waits_for_a_person(Some(3), &["media-jam"]));
        assert!(!waits_for_a_person(Some(3), &["marker-supply-low-warning"]));
    }
}
