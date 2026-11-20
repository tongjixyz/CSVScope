//! Local, bounded diagnostics. Disk writes never run on the calling thread.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const FILE_LIMIT: u64 = 5 * 1024 * 1024;
const FILE_COUNT: usize = 10;
static LOGGER: OnceLock<Arc<Logger>> = OnceLock::new();
static NEXT_OPERATION: AtomicU64 = AtomicU64::new(1);

pub fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

enum Message { Event(Value), Flush(mpsc::Sender<()>) }

struct ActiveOperation {
    name: &'static str,
    started: Instant,
    details: Value,
    thread: String,
}

struct Logger {
    sender: SyncSender<Message>,
    directory: PathBuf,
    initial_file: PathBuf,
    dropped: Arc<AtomicU64>,
    active: Mutex<BTreeMap<u64, ActiveOperation>>,
}

struct LogWriter {
    directory: PathBuf,
    stem: String,
    part: u32,
    file: File,
    _lease: File,
    bytes: u64,
    limit: u64,
}

impl LogWriter {
    fn new(directory: &Path, limit: u64) -> io::Result<(Self, PathBuf)> {
        fs::create_dir_all(directory)?;
        let stem = format!("csvscope-{}-{}", unix_ms(), std::process::id());
        let path = directory.join(format!("{stem}-0.log"));
        let (file, lease) = Self::open(&path)?;
        let writer = Self { directory: directory.into(), stem, part: 0, file, _lease: lease, bytes: 0, limit };
        writer.prune();
        Ok((writer, path))
    }
    fn open(path: &Path) -> io::Result<(File, File)> {
        // Windows byte-range locks also prevent readers. Lock a sidecar so the
        // actual log can be read/copied while the application is still hung.
        let lease = OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .open(path.with_extension("lock"))?;
        fs2::FileExt::try_lock_exclusive(&lease)?;
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(path)?;
        Ok((file, lease))
    }
    fn prune(&self) {
        let Ok(entries) = fs::read_dir(&self.directory) else { return };
        let mut files: Vec<_> = entries.filter_map(Result::ok).filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with("csvscope-") && name.ends_with(".log")
                && entry.file_type().is_ok_and(|kind| kind.is_file())
        }).collect();
        files.sort_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok());
        let surplus = files.len().saturating_sub(FILE_COUNT);
        for entry in files.into_iter().take(surplus) {
            // Another running instance's log must stay intact.
            let lease_path = entry.path().with_extension("lock");
            if let Ok(file) = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&lease_path) {
                if fs2::FileExt::try_lock_exclusive(&file).is_ok() {
                    let _ = fs::remove_file(entry.path());
                    drop(file);
                    let _ = fs::remove_file(lease_path);
                }
            }
        }
    }
    fn write(&mut self, value: &Value) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        if self.bytes > 0 && self.bytes + bytes.len() as u64 > self.limit {
            self.part += 1;
            let (file, lease) = Self::open(&self.directory.join(format!("{}-{}.log", self.stem, self.part)))?;
            self.file = file;
            self._lease = lease;
            self.bytes = 0;
            self.prune();
        }
        self.file.write_all(&bytes)?;
        self.bytes += bytes.len() as u64;
        Ok(())
    }
}

impl Logger {
    fn new(directory: &Path) -> io::Result<Arc<Self>> {
        let (mut writer, initial_file) = LogWriter::new(directory, FILE_LIMIT)?;
        let (sender, receiver) = mpsc::sync_channel(2048);
        let dropped = Arc::new(AtomicU64::new(0));
        let lost = dropped.clone();
        thread::Builder::new().name("diagnostic-writer".into()).spawn(move || {
            while let Ok(message) = receiver.recv() {
                let count = lost.swap(0, Ordering::Relaxed);
                if count > 0 {
                    let _ = writer.write(&event_value("warn", "log_queue_overflow", json!({ "dropped": count })));
                }
                match message {
                    Message::Event(value) => {
                        if let Err(error) = writer.write(&value) {
                            // A full/unplugged log volume must never stall application work.
                            lost.fetch_add(1, Ordering::Relaxed);
                            eprintln!("CSVScope diagnostic write failed: {error}");
                        }
                    }
                    Message::Flush(done) => { let _ = writer.file.flush(); let _ = done.send(()); }
                }
            }
        })?;
        Ok(Arc::new(Self { sender, directory: directory.into(), initial_file, dropped, active: Mutex::new(BTreeMap::new()) }))
    }
    fn send(&self, value: Value) {
        if self.sender.try_send(Message::Event(value)).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn event_value(level: &str, event: &str, details: Value) -> Value {
    json!({
        "timestampUnixMs": unix_ms(), "level": level, "event": event,
        "pid": std::process::id(), "thread": format!("{:?}", thread::current().id()),
        "details": details,
    })
}

pub fn init(directory: &Path) -> io::Result<PathBuf> {
    if let Some(logger) = LOGGER.get() { return Ok(logger.initial_file.clone()); }
    let logger = Logger::new(directory)?;
    let path = logger.initial_file.clone();
    let _ = LOGGER.set(logger);
    Ok(path)
}

pub fn directory() -> Option<PathBuf> { LOGGER.get().map(|logger| logger.directory.clone()) }

pub fn record(level: &str, event: &str, details: Value) {
    if let Some(logger) = LOGGER.get() { logger.send(event_value(level, event, details)); }
}

/// Only used at process exit and by diagnostic tests, never by the UI loop.
pub fn flush() {
    if let Some(logger) = LOGGER.get() {
        let (done, receiver) = mpsc::channel();
        if logger.sender.try_send(Message::Flush(done)).is_ok() {
            let _ = receiver.recv_timeout(Duration::from_secs(2));
        }
    }
}

pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let details = json!({
            "message": info.to_string(),
            "backtrace": std::backtrace::Backtrace::force_capture().to_string(),
            "activeOperations": active_operations(),
        });
        record("error", "rust_panic", details.clone());
        // Keep a synchronous emergency copy even if the process exits before the queue drains.
        if let Some(directory) = directory() {
            let path = directory.join(format!("panic-{}-{}.log", unix_ms(), std::process::id()));
            if let Ok(mut file) = OpenOptions::new().create_new(true).write(true).open(path) {
                if let Ok(bytes) = serde_json::to_vec(&event_value("error", "rust_panic", details)) {
                    let _ = file.write_all(&bytes);
                    let _ = file.write_all(b"\n");
                    let _ = file.sync_all();
                }
            }
        }
        previous(info);
    }));
}

pub struct Operation {
    id: u64,
    name: &'static str,
    started: Instant,
    quiet: bool,
    finished: bool,
    details: Value,
}

impl Operation {
    pub fn start(name: &'static str, details: Value, quiet: bool) -> Self {
        let id = NEXT_OPERATION.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        if let Some(logger) = LOGGER.get() {
            logger.active.lock().unwrap_or_else(|e| e.into_inner()).insert(id, ActiveOperation {
                name, started, details: details.clone(), thread: format!("{:?}", thread::current().id()),
            });
            if !quiet { record("info", "operation_started", json!({ "id": id, "operation": name, "context": details })); }
        }
        Self { id, name, started, quiet, finished: false, details }
    }
    pub fn finish(&mut self, error: Option<String>) {
        if self.finished { return; }
        self.finished = true;
        if let Some(logger) = LOGGER.get() {
            logger.active.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
            let elapsed = self.started.elapsed().as_millis() as u64;
            if !self.quiet || elapsed >= 1000 || error.is_some() {
                record(if error.is_some() { "error" } else if elapsed >= 1000 { "warn" } else { "info" },
                    "operation_finished", json!({
                        "id": self.id, "operation": self.name, "elapsedMs": elapsed,
                        "context": self.details, "outcome": if error.is_some() { "error" } else { "ok" }, "error": error,
                    }));
            }
        }
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if !self.finished { self.finish(Some("操作未正常结束（线程异常或提前返回）".into())); }
    }
}

pub fn measure<T, E: std::fmt::Display>(
    name: &'static str, details: Value, quiet: bool, action: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let mut operation = Operation::start(name, details, quiet);
    let result = action();
    operation.finish(result.as_ref().err().map(ToString::to_string));
    result
}

/// Never waits for an application operation or for the diagnostic registry itself.
pub fn active_operations() -> Value {
    let Some(logger) = LOGGER.get() else { return json!([]) };
    let Ok(active) = logger.active.try_lock() else { return json!({ "registryBusy": true }) };
    json!(active.iter().map(|(id, op)| json!({
        "id": id, "operation": op.name, "elapsedMs": op.started.elapsed().as_millis() as u64,
        "thread": op.thread, "context": op.details,
    })).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_writer_rotates_and_keeps_valid_json_lines() {
        let temp = tempfile::tempdir().unwrap();
        let (mut writer, _) = LogWriter::new(temp.path(), 200).unwrap();
        for index in 0..25 { writer.write(&event_value("info", "test", json!({ "index": index }))).unwrap(); }
        drop(writer);
        let files: Vec<_> = fs::read_dir(temp.path()).unwrap().map(Result::unwrap)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log")).collect();
        assert!(files.len() <= FILE_COUNT);
        let mut indices = vec![];
        for file in files {
            for line in fs::read_to_string(file.path()).unwrap().lines() {
                let value: Value = serde_json::from_str(line).unwrap();
                indices.push(value["details"]["index"].as_u64().unwrap());
            }
        }
        assert!(indices.contains(&24), "latest event must survive rotation");
    }

    #[test]
    fn queued_errors_are_written_while_registry_is_locked() {
        let temp = tempfile::tempdir().unwrap();
        let logger = Logger::new(temp.path()).unwrap();
        let _held = logger.active.lock().unwrap();
        let (done, receiver) = mpsc::channel();
        logger.sender.try_send(Message::Event(event_value("error", "test_error", json!({ "message": "错误\n第二行" })))).unwrap();
        logger.sender.try_send(Message::Flush(done)).unwrap();
        receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        let value: Value = serde_json::from_str(fs::read_to_string(&logger.initial_file).unwrap().trim()).unwrap();
        assert_eq!(value["details"]["message"], "错误\n第二行");
    }

    #[test]
    fn panic_hook_preserves_an_emergency_backtrace() {
        let temp = tempfile::tempdir().unwrap();
        init(temp.path()).unwrap();
        install_panic_hook();
        let caught = std::panic::catch_unwind(|| panic!("diagnostic-panic-probe"));
        assert!(caught.is_err());
        let panic_file = fs::read_dir(temp.path()).unwrap().map(Result::unwrap)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("panic-")).unwrap();
        let value: Value = serde_json::from_slice(&fs::read(panic_file.path()).unwrap()).unwrap();
        assert_eq!(value["event"], "rust_panic");
        assert!(value["details"]["message"].as_str().unwrap().contains("diagnostic-panic-probe"));
        assert!(!value["details"]["backtrace"].as_str().unwrap().is_empty());
        flush();
    }
}
