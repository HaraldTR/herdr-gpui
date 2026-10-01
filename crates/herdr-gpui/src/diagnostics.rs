//! Client logs, stored on disk rather than in memory. Capture never blocks the
//! emitting thread: each event is serialized to one JSON line of at most 4096
//! bytes and offered to a bounded queue drained by a dedicated writer thread,
//! which appends to `<state>/herdr/gpui/logs/herdr-gpui.jsonl` and rotates it to
//! `herdr-gpui.1.jsonl` beyond 16 MiB. A full queue or failed write drops lines
//! and counts them. No uploads, environment filters, or stderr. Span context
//! includes names, not fields; arbitrary field Debug implementations must
//! cooperate with fmt errors.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    cell::Cell,
    collections::VecDeque,
    fmt::{self, Write as _},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
};
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

const MAX_BYTES: usize = 4096;
const TIMESTAMP_FORMAT: &str = "[%Y-%m-%d %H:%M:%S]";
const FILE_NAME: &str = "herdr-gpui.jsonl";
const PREVIOUS_FILE_NAME: &str = "herdr-gpui.1.jsonl";
/// Lines waiting for the writer; beyond this, capture drops rather than blocks.
const QUEUE: usize = 4096;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// The writer issues one write per batch of at most this many bytes.
const BATCH_BYTES: usize = 256 * 1024;
/// Newest records a reader keeps; older ones remain only on disk.
pub(crate) const TAIL_RECORDS: usize = 5000;
/// A reader opening an existing file starts this far from its end.
const TAIL_BYTES: u64 = 4 * 1024 * 1024;
const READ_CHUNK: usize = 64 * 1024;
/// Longer lines are not ours; skip them without buffering.
const MAX_LINE: usize = 64 * 1024;
static SINK: OnceLock<Sink> = OnceLock::new();
thread_local! { static FORMATTING: Cell<bool> = const { Cell::new(false) }; }

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "event")]
pub(crate) struct Record {
    #[serde(with = "level_json")]
    pub(crate) level: Level,
    /// Local wall time at capture, retained unchanged for display and export.
    pub(crate) timestamp: String,
    pub(crate) target: String,
    pub(crate) namespace: String,
    pub(crate) message: String,
    pub(crate) fields: Map<String, Value>,
    /// Leaf first, names only. Never retain raw span fields.
    pub(crate) spans: Vec<String>,
    pub(crate) truncated: bool,
}

impl Record {
    pub(crate) fn body(&self) -> String {
        let mut text = self.target.clone();
        for span in &self.spans {
            let _ = write!(text, " [{span}]");
        }
        if !self.message.is_empty() {
            let _ = write!(text, " {}", self.message);
        }
        for (key, value) in &self.fields {
            let _ = write!(text, " {key}={value}");
        }
        if self.truncated {
            text.push_str(" [truncated]");
        }
        text
    }

    pub(crate) fn line(&self) -> String {
        format!(
            "{} {:<5} {}",
            self.timestamp,
            self.level.as_str(),
            self.body()
        )
    }

    #[cfg(test)]
    pub(crate) fn fixture(level: Level, message: impl Into<String>) -> Self {
        Self {
            level,
            timestamp: "[2025-09-26 15:03:45]".into(),
            target: String::new(),
            namespace: String::new(),
            message: message.into(),
            fields: Map::new(),
            spans: Vec::new(),
            truncated: false,
        }
    }
}

mod level_json {
    use super::*;

    pub(super) fn serialize<S: serde::Serializer>(
        level: &Level,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(level.as_str())
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Level, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Install once, before starting workers. An existing global subscriber is an
/// error, never silently replaced. No tracing-log bridge is installed. Without a
/// state directory or writer thread, every event counts as dropped.
pub(crate) fn init() -> Result<(), tracing::subscriber::SetGlobalDefaultError> {
    let (lines, queued) = mpsc::sync_channel(QUEUE);
    let counters = Arc::new(Counters::default());
    let path = crate::preferences::state_dir()
        .map(|dir| dir.join("logs").join(FILE_NAME))
        .and_then(|path| {
            let writer_path = path.clone();
            let writer_counters = counters.clone();
            thread::Builder::new()
                .name("gpui-log-writer".into())
                .spawn(move || write_lines(writer_path, queued, &writer_counters))
                .ok()
                .map(|_| path)
        });
    let _ = SINK.set(Sink {
        counters: counters.clone(),
        path,
    });
    tracing::subscriber::set_global_default(subscriber(Capture { lines, counters }))
}

struct Sink {
    counters: Arc<Counters>,
    path: Option<PathBuf>,
}

/// The file the writer appends to, when one could be started.
pub(crate) fn path() -> Option<&'static Path> {
    SINK.get()?.path.as_deref()
}

/// A change hint: advances after each written batch and each dropped line.
pub(crate) fn generation() -> u64 {
    SINK.get()
        .map_or(0, |sink| sink.counters.generation.load(Ordering::Acquire))
}

/// Lines this process failed to queue or write.
pub(crate) fn dropped() -> u64 {
    SINK.get()
        .map_or(0, |sink| sink.counters.dropped.load(Ordering::Relaxed))
}

#[derive(Default)]
struct Counters {
    generation: AtomicU64,
    dropped: AtomicU64,
}

impl Counters {
    fn lose(&self, lines: u64) {
        self.dropped.fetch_add(lines, Ordering::Relaxed);
        self.generation.fetch_add(1, Ordering::Release);
    }
}

fn write_lines(path: PathBuf, queued: Receiver<Vec<u8>>, counters: &Counters) {
    let mut log = LogFile {
        path,
        file: None,
        limit: MAX_FILE_BYTES,
    };
    let mut batch = Vec::new();
    while let Ok(first) = queued.recv() {
        batch.extend_from_slice(&first);
        let mut lines = 1;
        while batch.len() < BATCH_BYTES
            && let Ok(line) = queued.try_recv()
        {
            batch.extend_from_slice(&line);
            lines += 1;
        }
        if log.append(&batch).is_err() {
            counters.lose(lines);
        } else {
            counters.generation.fetch_add(1, Ordering::Release);
        }
        batch.clear();
    }
}

struct LogFile {
    path: PathBuf,
    /// The open file and its length; closed after a failure so the next batch reopens it.
    file: Option<(File, u64)>,
    limit: u64,
}

impl LogFile {
    /// Appends whole lines, rotating first when they would overflow a non-empty file.
    fn append(&mut self, lines: &[u8]) -> io::Result<()> {
        let incoming = lines.len() as u64;
        let (mut file, mut len) = match self.file.take() {
            Some(open) => open,
            None => {
                let file = self.open()?;
                let len = file.metadata()?.len();
                (file, len)
            }
        };
        if len > 0 && len.saturating_add(incoming) > self.limit {
            // Windows cannot rename an open file.
            drop(file);
            self.rotate()?;
            file = self.open()?;
            len = 0;
        }
        file.write_all(lines)?;
        self.file = Some((file, len + incoming));
        Ok(())
    }

    fn rotate(&self) -> io::Result<()> {
        fs::rename(&self.path, self.path.with_file_name(PREVIOUS_FILE_NAME))
    }

    fn open(&self) -> io::Result<File> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        // Logs can name local paths and hosts; keep them private to the user.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(&self.path)
    }
}

/// Incrementally reads the newest records of a log file, keeping at most
/// [`TAIL_RECORDS`]. Unparseable lines are skipped. A file shorter than the
/// read position was rotated or truncated and is read again from its start.
pub(crate) struct Tail {
    path: PathBuf,
    offset: Option<u64>,
    partial: Vec<u8>,
    /// Skipping to the next newline: a line started before the read position or ran too long.
    discarding: bool,
    records: VecDeque<Arc<Record>>,
}

impl Tail {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: None,
            partial: Vec::new(),
            discarding: false,
            records: VecDeque::new(),
        }
    }

    /// Reads whatever was appended since the last call. A missing file is empty.
    pub(crate) fn read(&mut self) -> io::Result<()> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let len = file.metadata()?.len();
        let mut offset = match self.offset {
            Some(offset) if offset <= len => offset,
            Some(_) => {
                self.partial.clear();
                self.discarding = false;
                0
            }
            None => {
                let start = len.saturating_sub(TAIL_BYTES);
                self.discarding = start > 0;
                start
            }
        };
        file.seek(SeekFrom::Start(offset))?;
        let mut chunk = vec![0; READ_CHUNK];
        // Stop at the length observed above; later appends are read next time.
        while offset < len {
            let limit =
                usize::try_from(len - offset).map_or(READ_CHUNK, |rest| rest.min(READ_CHUNK));
            let read = file.read(&mut chunk[..limit])?;
            if read == 0 {
                break;
            }
            offset += read as u64;
            self.consume(&chunk[..read]);
        }
        self.offset = Some(offset);
        Ok(())
    }

    fn consume(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let (line, complete, rest) = match bytes.iter().position(|byte| *byte == b'\n') {
                Some(end) => (&bytes[..end], true, &bytes[end + 1..]),
                None => (bytes, false, &[][..]),
            };
            bytes = rest;
            if !self.discarding {
                if self.partial.len() + line.len() > MAX_LINE {
                    self.partial.clear();
                    self.discarding = true;
                } else {
                    self.partial.extend_from_slice(line);
                }
            }
            if !complete {
                continue;
            }
            if !self.discarding
                && let Ok(record) = serde_json::from_slice::<Record>(&self.partial)
            {
                if self.records.len() == TAIL_RECORDS {
                    self.records.pop_front();
                }
                self.records.push_back(Arc::new(record));
            }
            self.partial.clear();
            self.discarding = false;
        }
    }

    /// Oldest first.
    pub(crate) fn records(&self) -> Vec<Arc<Record>> {
        self.records.iter().cloned().collect()
    }
}

fn app_target(target: &str) -> bool {
    ["herdr_gpui", "herdr_client", "herdr_protocol"]
        .iter()
        .any(|root| {
            target == *root
                || target
                    .strip_prefix(root)
                    .is_some_and(|rest| rest.starts_with("::"))
        })
}

fn subscriber(capture: Capture) -> impl Subscriber + Send + Sync {
    tracing_subscriber::registry().with(capture.with_filter(tracing_subscriber::filter::filter_fn(
        |meta| app_target(meta.target()),
    )))
}

struct Capture {
    lines: SyncSender<Vec<u8>>,
    counters: Arc<Counters>,
}
struct FormattingGuard;
impl Drop for FormattingGuard {
    fn drop(&mut self) {
        FORMATTING.set(false);
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if FORMATTING.replace(true) {
            self.counters.lose(1);
            return;
        }
        let _guard = FormattingGuard;
        let timestamp = chrono::Local::now().format(TIMESTAMP_FORMAT).to_string();
        let meta = event.metadata();
        let mut target = Bounded::new(256);
        let _ = target.write_str(meta.target());
        let mut record = Record {
            level: *meta.level(),
            timestamp,
            target: target.text,
            namespace: meta.target().split("::").next().unwrap_or_default().into(),
            message: String::new(),
            fields: Map::new(),
            spans: Vec::new(),
            truncated: target.truncated,
        };
        if let Some(scope) = ctx.event_scope(event) {
            // Leaf first avoids allocating/reversing an arbitrarily deep scope.
            for (index, span) in scope.take(17).enumerate() {
                if index == 16 {
                    record.truncated = true;
                    break;
                }
                let mut name = Bounded::new(64);
                let _ = name.write_str(span.name());
                record.truncated |= name.truncated;
                record.spans.push(name.text);
            }
        }
        let Ok(base) = serde_json::to_vec(&record) else {
            self.counters.lose(1);
            return;
        };
        let mut visitor = Fields {
            record: &mut record,
            remaining: MAX_BYTES.saturating_sub(base.len()),
            count: 0,
        };
        event.record(&mut visitor);
        let Ok(mut line) = serde_json::to_vec(&record) else {
            self.counters.lose(1);
            return;
        };
        line.push(b'\n');
        if self.lines.try_send(line).is_err() {
            self.counters.lose(1);
        }
    }
}

struct Bounded {
    text: String,
    truncated: bool,
    remaining: usize,
}

impl Bounded {
    fn new(remaining: usize) -> Self {
        Self {
            text: String::new(),
            truncated: false,
            remaining,
        }
    }
}

impl fmt::Write for Bounded {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.truncated {
            return Err(fmt::Error);
        }
        // Sanitize while copying, never allocate a full formatted field. Stop
        // scanning once full, even for megabytes of untrusted UTF-8/control data.
        for ch in value.chars() {
            let ch = if ch.is_control() { ' ' } else { ch };
            let bytes = if ch == '"' || ch == '\\' {
                2
            } else {
                ch.len_utf8()
            };
            if bytes > self.remaining {
                self.truncated = true;
                return Err(fmt::Error);
            }
            self.text.push(ch);
            self.remaining -= bytes;
        }
        Ok(())
    }
}

struct Fields<'a> {
    record: &'a mut Record,
    remaining: usize,
    count: usize,
}

impl Fields<'_> {
    fn budget(&mut self, field: &Field) -> Option<usize> {
        // Field names are static but may still be very long. Reject rather than
        // truncate keys, which could merge unrelated fields.
        let overhead = field.name().len().saturating_mul(6).saturating_add(8);
        if self.count >= 32 || field.name().len() > 128 || overhead >= self.remaining {
            self.record.truncated = true;
            return None;
        }
        self.count += 1;
        self.remaining -= overhead;
        Some(self.remaining)
    }

    fn insert(&mut self, field: &Field, value: Value) {
        let Ok(encoded) = serde_json::to_vec(&value) else {
            return;
        };
        if encoded.len() > self.remaining {
            self.record.truncated = true;
            return;
        }
        self.remaining -= encoded.len();
        if field.name() == "message" {
            self.record.message = match value {
                Value::String(text) => text,
                other => other.to_string(),
            };
        } else {
            self.record.fields.insert(field.name().into(), value);
        }
    }

    fn scalar(&mut self, field: &Field, value: Value) {
        if self.budget(field).is_some() {
            self.insert(field, value);
        }
    }
}

impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let Some(budget) = self.budget(field) else {
            return;
        };
        let mut text = Bounded::new(budget.saturating_sub(2));
        let _ = write!(text, "{value:?}");
        self.record.truncated |= text.truncated;
        self.insert(field, Value::String(text.text));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        let Some(budget) = self.budget(field) else {
            return;
        };
        let mut text = Bounded::new(budget.saturating_sub(2));
        let _ = text.write_str(value);
        self.record.truncated |= text.truncated;
        self.insert(field, Value::String(text.text));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.scalar(field, value.into());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.scalar(field, value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.scalar(field, value.into());
    }
    fn record_i128(&mut self, field: &Field, value: i128) {
        if let Some(number) = serde_json::Number::from_i128(value) {
            self.scalar(field, Value::Number(number));
        } else {
            self.record_debug(field, &value);
        }
    }
    fn record_u128(&mut self, field: &Field, value: u128) {
        if let Some(number) = serde_json::Number::from_u128(value) {
            self.scalar(field, Value::Number(number));
        } else {
            self.record_debug(field, &value);
        }
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        if value.is_finite() {
            self.scalar(field, value.into());
        } else {
            self.record_debug(field, &value);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_and_left_aligned_levels_have_exact_columns() {
        let time = chrono::DateTime::parse_from_rfc3339("2025-09-26T15:03:45-07:00").unwrap();
        let timestamp = time.format(TIMESTAMP_FORMAT).to_string();
        assert_eq!(timestamp, "[2025-09-26 15:03:45]");
        for (level, padded) in [
            (Level::TRACE, "TRACE"),
            (Level::DEBUG, "DEBUG"),
            (Level::INFO, "INFO "),
            (Level::WARN, "WARN "),
            (Level::ERROR, "ERROR"),
        ] {
            let mut record = Record::fixture(level, "message");
            record.target = "target".into();
            assert_eq!(
                record.line(),
                format!("[2025-09-26 15:03:45] {padded} target message")
            );
            assert_eq!(&record.line()[28..], "target message");
        }
    }

    fn sink(capacity: usize) -> (Capture, Receiver<Vec<u8>>, Arc<Counters>) {
        let (lines, queued) = mpsc::sync_channel(capacity);
        let counters = Arc::new(Counters::default());
        let capture = Capture {
            lines,
            counters: counters.clone(),
        };
        (capture, queued, counters)
    }

    fn queued_records(queued: &Receiver<Vec<u8>>) -> Vec<Record> {
        queued
            .try_iter()
            .map(|line| {
                assert_eq!(line.last(), Some(&b'\n'));
                assert_eq!(line.iter().filter(|byte| **byte == b'\n').count(), 1);
                serde_json::from_slice(&line).unwrap()
            })
            .collect()
    }

    fn counts(counters: &Counters) -> (u64, u64) {
        (
            counters.generation.load(Ordering::Acquire),
            counters.dropped.load(Ordering::Relaxed),
        )
    }

    fn line(message: &str) -> Vec<u8> {
        let mut line = serde_json::to_vec(&Record::fixture(Level::INFO, message)).unwrap();
        line.push(b'\n');
        line
    }

    fn messages(tail: &Tail) -> Vec<String> {
        tail.records()
            .iter()
            .map(|record| record.message.clone())
            .collect()
    }

    #[test]
    fn public_api_is_available_without_installing_a_global_subscriber() {
        let _init = init;
        let _ = (generation(), dropped(), path());
    }

    #[test]
    fn json_schema_preserves_typed_fields_and_roundtrips() {
        let (capture, queued, _) = sink(QUEUE);
        tracing::subscriber::with_default(subscriber(capture), || {
            let span = tracing::info_span!(target: "herdr_gpui", "paint", password = "secret");
            let _entered = span.enter();
            tracing::info!(target: "herdr_gpui::terminal_painter",
                ready = true, signed = -7_i64, unsigned = u64::MAX, ratio = 1.25_f64,
                text = "quoted \"text\"", debug = ?[1, 2], large = u128::MAX,
                nonfinite = f64::INFINITY, "Paint complete");
        });
        let rows = queued_records(&queued);
        let row = &rows[0];
        let encoded = serde_json::to_string(row).unwrap();
        let json: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(json["type"], "event");
        assert_eq!(json["level"], "INFO");
        assert_eq!(json["namespace"], "herdr_gpui");
        assert_eq!(json["target"], "herdr_gpui::terminal_painter");
        assert_eq!(json["message"], "Paint complete");
        assert_eq!(json["fields"]["ready"], true);
        assert_eq!(json["fields"]["signed"], -7);
        assert_eq!(json["fields"]["unsigned"], u64::MAX);
        assert_eq!(json["fields"]["ratio"], 1.25);
        assert_eq!(json["fields"]["text"], "quoted \"text\"");
        assert_eq!(json["fields"]["debug"], "[1, 2]");
        assert_eq!(json["fields"]["large"], u128::MAX.to_string());
        assert_eq!(json["fields"]["nonfinite"], "inf");
        assert_eq!(json["spans"], serde_json::json!(["paint"]));
        assert!(!encoded.contains("secret"));
        assert_eq!(serde_json::from_str::<Record>(&encoded).unwrap(), *row);
        assert!(serde_json::from_str::<Record>(&encoded.replace("INFO", "INVALID")).is_err());
    }

    #[test]
    fn json_escaping_and_many_fields_share_one_record_budget() {
        let (capture, queued, _) = sink(QUEUE);
        let huge = "\"\\\n\u{1f980}".repeat(100_000);
        tracing::subscriber::with_default(subscriber(capture), || {
            tracing::info!(target: "herdr_client", a = huge.as_str(), b = huge.as_str(), "{}", huge);
            tracing::info!(target: "herdr_client",
                a01=1,a02=2,a03=3,a04=4,a05=5,a06=6,a07=7,a08=8,a09=9,a10=10,
                a11=1,a12=2,a13=3,a14=4,a15=5,a16=6,a17=7,a18=8,a19=9,a20=10,
                a21=1,a22=2,a23=3,a24=4,a25=5,a26=6,a27=7,a28=8,a29=9,a30=10,
                a31=1,a32=2,a33=3,a34=4);
        });
        let lines: Vec<_> = queued.try_iter().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            // The budget covers the JSON; the newline is framing.
            assert!(line.len() <= MAX_BYTES + 1, "{}", line.len());
            let row: Record = serde_json::from_slice(line).unwrap();
            assert!(row.truncated);
            assert!(row.fields.len() <= 32);
        }
        let row: Record = serde_json::from_slice(&lines[1]).unwrap();
        assert_eq!(row.fields.len(), 32);
    }

    #[test]
    fn levels_targets_and_span_context() {
        let (capture, queued, counters) = sink(QUEUE);
        tracing::subscriber::with_default(subscriber(capture), || {
            let span =
                tracing::info_span!(target: "herdr_client", "connection", secret = "not retained");
            let _entered = span.enter();
            tracing::trace!(target: "herdr_client::worker", "trace");
            tracing::debug!(target: "herdr_gpui", "debug");
            tracing::info!(target: "herdr_protocol", "info");
            tracing::warn!(target: "herdr_client", "warn");
            tracing::error!(target: "herdr_gpui", "error");
            tracing::error!(target: "herdr_gpui_impostor", "excluded");
            tracing::error!(target: "gpui", "excluded");
        });
        let records = queued_records(&queued);
        assert_eq!(counts(&counters), (0, 0));
        assert_eq!(
            records.iter().map(|r| r.level).collect::<Vec<_>>(),
            [
                Level::TRACE,
                Level::DEBUG,
                Level::INFO,
                Level::WARN,
                Level::ERROR
            ]
        );
        for record in records {
            assert_eq!(record.spans, ["connection"]);
            assert!(!record.body().contains("not retained"));
            assert!(
                chrono::NaiveDateTime::parse_from_str(&record.timestamp, "[%Y-%m-%d %H:%M:%S]")
                    .is_ok()
            );
        }
    }

    #[test]
    fn bounded_utf8_controls_and_debug_stream() {
        struct Huge;
        impl fmt::Debug for Huge {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                for _ in 0..1_000_000 {
                    f.write_str("\u{1f980}\n\0")?;
                }
                panic!("formatter should stop on budget exhaustion")
            }
        }
        let (capture, queued, _) = sink(QUEUE);
        tracing::subscriber::with_default(subscriber(capture), || {
            tracing::info!(target: "herdr_gpui", value = ?Huge);
        });
        let records = queued_records(&queued);
        assert!(serde_json::to_vec(&records[0]).unwrap().len() <= MAX_BYTES);
        assert!(records[0].truncated);
        assert!(!records[0].body().chars().any(char::is_control));
    }

    #[test]
    fn filtered_and_reentrant_events_never_format_fields() {
        struct MustNotFormat;
        impl fmt::Debug for MustNotFormat {
            fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
                panic!("filtered or reentrant event formatted a field")
            }
        }
        let (capture, queued, counters) = sink(QUEUE);
        tracing::subscriber::with_default(subscriber(capture), || {
            tracing::error!(target: "dependency", value = ?MustNotFormat);
            FORMATTING.set(true);
            let _guard = FormattingGuard;
            tracing::error!(target: "herdr_client", value = ?MustNotFormat);
        });
        assert!(queued_records(&queued).is_empty());
        assert_eq!(counts(&counters), (1, 1));
        assert!(!FORMATTING.get());
    }

    #[test]
    fn a_full_queue_or_missing_writer_drops_without_blocking() {
        let (capture, queued, counters) = sink(1);
        tracing::subscriber::with_default(subscriber(capture), || {
            for i in 0..3 {
                tracing::info!(target: "herdr_gpui", i, "queued");
            }
        });
        assert_eq!(queued_records(&queued).len(), 1);
        assert_eq!(counts(&counters), (2, 2));

        let (capture, queued, counters) = sink(QUEUE);
        drop(queued);
        tracing::subscriber::with_default(subscriber(capture), || {
            tracing::info!(target: "herdr_gpui", "no writer");
        });
        assert_eq!(counts(&counters), (1, 1));
    }

    #[test]
    fn concurrent_writers_reach_disk_or_count_as_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join(FILE_NAME);
        let (capture, queued, counters) = sink(QUEUE);
        let writer = {
            let path = path.clone();
            let counters = counters.clone();
            thread::spawn(move || write_lines(path, queued, &counters))
        };
        let dispatch = tracing::Dispatch::new(subscriber(capture));
        thread::scope(|scope| {
            for _ in 0..8 {
                let dispatch = dispatch.clone();
                scope.spawn(move || {
                    tracing::dispatcher::with_default(&dispatch, || {
                        for i in 0..2000 {
                            tracing::trace!(target: "herdr_client", iteration = i, "concurrent");
                        }
                    });
                });
            }
        });
        // The writer exits once every sender, owned by the subscriber, is gone.
        drop(dispatch);
        writer.join().unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let written = text.lines().count() as u64;
        for line in text.lines() {
            assert_eq!(
                serde_json::from_str::<Record>(line).unwrap().message,
                "concurrent"
            );
        }
        assert!(written > 0);
        assert_eq!(written + counts(&counters).1, 16000);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn log_files_rotate_before_overflowing_and_reopen_after_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let previous = dir.path().join(PREVIOUS_FILE_NAME);
        let mut log = LogFile {
            path: path.clone(),
            file: None,
            limit: 10,
        };
        log.append(b"aaaa\n").unwrap();
        log.append(b"bbbb\n").unwrap();
        log.append(b"cccc\n").unwrap();
        assert_eq!(fs::read(&previous).unwrap(), b"aaaa\nbbbb\n");
        assert_eq!(fs::read(&path).unwrap(), b"cccc\n");
        // A single oversized batch still lands, alone, in a fresh file.
        log.append(b"0123456789abcdef\n").unwrap();
        assert_eq!(fs::read(&previous).unwrap(), b"cccc\n");
        assert_eq!(fs::read(&path).unwrap(), b"0123456789abcdef\n");

        // A reopened process measures the existing file before appending.
        let mut reopened = LogFile {
            path: path.clone(),
            file: None,
            limit: 20,
        };
        reopened.append(b"dddd\n").unwrap();
        assert_eq!(fs::read(&previous).unwrap(), b"0123456789abcdef\n");
        assert_eq!(fs::read(&path).unwrap(), b"dddd\n");

        let blocked = LogFile {
            path: dir.path().join("file").join(FILE_NAME),
            file: None,
            limit: 20,
        };
        fs::write(dir.path().join("file"), b"").unwrap();
        let mut blocked = blocked;
        assert!(blocked.append(b"eeee\n").is_err());
        assert!(blocked.file.is_none());
        fs::remove_file(dir.path().join("file")).unwrap();
        blocked.append(b"eeee\n").unwrap();
        assert_eq!(
            fs::read(dir.path().join("file").join(FILE_NAME)).unwrap(),
            b"eeee\n"
        );
    }

    #[test]
    fn tails_read_appended_complete_lines_and_skip_foreign_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let mut tail = Tail::new(&path);
        tail.read().unwrap();
        assert!(tail.records().is_empty());

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let second = line("second");
        file.write_all(&line("first")).unwrap();
        file.write_all(b"not json\n").unwrap();
        file.write_all(&vec![b'x'; MAX_LINE * 2]).unwrap();
        file.write_all(b"\n").unwrap();
        file.write_all(&second[..10]).unwrap();
        tail.read().unwrap();
        assert_eq!(messages(&tail), ["first"]);
        file.write_all(&second[10..]).unwrap();
        tail.read().unwrap();
        assert_eq!(messages(&tail), ["first", "second"]);

        // Rotation leaves a shorter file: read it from the start, keep history.
        fs::write(&path, line("rotated")).unwrap();
        tail.read().unwrap();
        assert_eq!(messages(&tail), ["first", "second", "rotated"]);
    }

    #[test]
    fn tails_start_near_the_end_and_keep_a_bounded_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let record = line("filler");
        let count = TAIL_BYTES as usize / record.len() + 2;
        let mut text = Vec::with_capacity(count * record.len());
        for _ in 0..count {
            text.extend_from_slice(&record);
        }
        text.extend_from_slice(&line("newest"));
        fs::write(&path, &text).unwrap();
        let mut tail = Tail::new(&path);
        tail.read().unwrap();
        let records = tail.records();
        // Only the last TAIL_BYTES are read, and only the newest records kept.
        assert!(count > TAIL_RECORDS);
        assert_eq!(records.len(), TAIL_RECORDS);
        assert_eq!(records.last().unwrap().message, "newest");

        let mut tail = Tail::new(&path);
        for _ in 0..TAIL_RECORDS + 3 {
            tail.consume(&line("more"));
        }
        tail.consume(&line("last"));
        assert_eq!(tail.records().len(), TAIL_RECORDS);
        assert_eq!(tail.records().last().unwrap().message, "last");
    }
}
