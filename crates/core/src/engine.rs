use crate::{
    diagnostics::{self, Operation},
    editing,
    filter::{column_name, compile_filters, compile_order},
    history::History,
    model::*,
    parser::{CsvStream, ReadOutcome, validate_width},
    store::{self, ResultStore},
};
use anyhow::{Context, Result, bail, ensure};
use duckdb::{Connection, params_from_iter};
use duckdb::arrow::{
    array::{ArrayRef, StringBuilder, UInt64Array},
    datatypes::{DataType, Field, Schema},
    record_batch::RecordBatch,
    util::display::array_value_to_string,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak, atomic::{AtomicBool, AtomicU64, Ordering}},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

fn id() -> String { Uuid::new_v4().simple().to_string() }
fn quote(value: &str) -> String { format!("'{}'", value.replace('\'', "''")) }
fn modified(path: &Path) -> Result<u128> { Ok(fs::metadata(path)?.modified()?.duration_since(UNIX_EPOCH)?.as_nanos()) }

struct Task {
    cancel: AtomicBool,
    interrupt: Mutex<Option<Arc<duckdb::InterruptHandle>>>,
    owner: String,
    priority: u8,
}
impl Task {
    fn new(owner: &str, priority: u8) -> Arc<Self> {
        Arc::new(Self { cancel: AtomicBool::new(false), interrupt: Mutex::new(None), owner: owner.into(), priority })
    }
    fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(handle) = self.interrupt.lock().unwrap().as_ref() { handle.interrupt(); }
    }
    fn check(&self) -> Result<()> { ensure!(!self.cancel.load(Ordering::Relaxed), "任务已取消"); Ok(()) }
    fn bind(&self, connection: &Connection) { *self.interrupt.lock().unwrap() = Some(connection.interrupt_handle()); }
}

struct Attachment {
    alias: String,
    connection: Weak<Mutex<Connection>>,
    _lease: File,
}
impl Drop for Attachment {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.upgrade() {
            let _ = connection.lock().unwrap().execute_batch(&format!("DETACH {}", self.alias));
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DatasetManifest {
    version: u32,
    engine: String,
    path: PathBuf,
    source_size: u64,
    source_modified: u128,
    hash: String,
    spec: ParseSpec,
    columns: Vec<Column>,
    rows: u64,
    ranges: Vec<(u64, u64)>,
    #[serde(default)]
    last_used: u64,
}
#[derive(Clone)]
struct Dataset { alias: String, manifest: DatasetManifest, _attachment: Arc<Attachment> }
struct Query {
    id: String,
    task: Arc<Task>,
    store: Arc<ResultStore>,
    view: ViewState,
    revision: u64,
    direct: bool,
}
struct SessionData {
    snapshot: SessionSnapshot,
    history: History,
    dataset: Option<Dataset>,
    query: Option<Arc<Query>>,
    baseline: EditState,
}
struct Session {
    data: Mutex<SessionData>,
    prepare: Mutex<Arc<Task>>,
}
struct CandidateTask {
    snapshot: Mutex<CandidateSnapshot>,
    task: Arc<Task>,
    store: Arc<ResultStore>,
    searches: Vec<String>,
}
struct ExportTask { snapshot: Mutex<ExportSnapshot>, task: Arc<Task> }

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ColorMetadata {
    version: u32,
    csv_hash: String,
    has_headers: bool,
    delimiter: Option<String>,
    colors: BTreeMap<u64, BTreeMap<usize, String>>,
}

fn color_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".csvscope.json");
    PathBuf::from(name)
}

pub struct Engine {
    pub config: EngineConfig,
    connection: Arc<Mutex<Connection>>,
    attachments: Mutex<BTreeMap<PathBuf, Weak<Attachment>>>,
    heavy: Mutex<()>,
    waiting: Mutex<BTreeMap<u64, (String, u8)>>,
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
    candidates: Mutex<BTreeMap<String, Arc<CandidateTask>>>,
    exports: Mutex<BTreeMap<String, Arc<ExportTask>>>,
    active: Mutex<Option<String>>,
    instance_dir: PathBuf,
    _instance_lock: Option<File>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Result<Arc<Self>> {
        fs::create_dir_all(config.cache_dir.join("datasets"))?;
        fs::create_dir_all(config.cache_dir.join("sessions"))?;
        fs::create_dir_all(config.cache_dir.join("leases"))?;
        for entry in fs::read_dir(config.cache_dir.join("sessions"))?.filter_map(Result::ok) {
            if !entry.file_type()?.is_dir() { continue; }
            let lock = OpenOptions::new().read(true).write(true).open(entry.path().join("instance.lock"));
            if let Ok(file) = lock {
                if fs2::FileExt::try_lock_exclusive(&file).is_ok() {
                    drop(file);
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }
        let instance_dir = config.cache_dir.join("sessions").join(id());
        fs::create_dir_all(&instance_dir)?;
        let instance_lock = OpenOptions::new().read(true).write(true).create_new(true).open(instance_dir.join("instance.lock"))?;
        fs2::FileExt::try_lock_exclusive(&instance_lock)?;
        let temporary = instance_dir.join("spill");
        fs::create_dir_all(&temporary)?;
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(&format!(
            "SET memory_limit='{}MB'; SET threads={}; SET temp_directory={};
             SET max_temp_directory_size='{}MB';
             SET autoinstall_known_extensions=false; SET autoload_known_extensions=false;
             SET enable_progress_bar=false; SET arrow_output_version='1.0';",
            config.memory_limit_mb, config.threads, quote(&temporary.to_string_lossy()),
            config.temp_limit_bytes / (1024 * 1024)))?;
        Ok(Arc::new(Self {
            config, connection: Arc::new(Mutex::new(connection)), attachments: Mutex::new(BTreeMap::new()),
            heavy: Mutex::new(()), waiting: Mutex::new(BTreeMap::new()),
            sessions: Mutex::new(BTreeMap::new()), candidates: Mutex::new(BTreeMap::new()),
            exports: Mutex::new(BTreeMap::new()), active: Mutex::new(None), instance_dir,
            _instance_lock: Some(instance_lock),
        }))
    }
    fn connection(&self) -> Result<Connection> {
        diagnostics::measure("engine.connection", serde_json::json!({}), true,
            || Ok(self.connection.lock().unwrap().try_clone()?))
    }
    fn session(&self, tab: &str) -> Result<Arc<Session>> {
        self.sessions.lock().unwrap().get(tab).cloned().context("文件标签已关闭")
    }
    fn permit<'a>(&'a self, task: &Task) -> Result<std::sync::MutexGuard<'a, ()>> {
        let mut operation = Operation::start("task.wait_for_slot",
            serde_json::json!({ "tabId": task.owner, "priority": task.priority }), true);
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let ticket = NEXT.fetch_add(1, Ordering::Relaxed);
        self.waiting.lock().unwrap().insert(ticket, (task.owner.clone(), task.priority));
        loop {
            if let Err(error) = task.check() {
                self.waiting.lock().unwrap().remove(&ticket);
                operation.finish(Some(error.to_string()));
                return Err(error);
            }
            let active = self.active.lock().unwrap().clone();
            let mut waiting = self.waiting.lock().unwrap();
            let first = waiting.iter().min_by_key(|(ticket, (owner, priority))|
                (Some(owner.as_str()) != active.as_deref(), *priority, **ticket)).map(|(ticket, _)| *ticket);
            if first == Some(ticket) {
                if let Ok(guard) = self.heavy.try_lock() {
                    waiting.remove(&ticket);
                    operation.finish(None);
                    return Ok(guard);
                }
            }
            drop(waiting);
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn disk_check(&self) -> Result<()> {
        if store::directory_size(&self.config.cache_dir) >= self.config.cache_limit_bytes
            || fs2::available_space(&self.config.cache_dir)? < self.config.reserve_bytes + 256 * 1024 * 1024 {
            self.evict_unused(false)?;
        }
        ensure!(fs2::available_space(&self.config.cache_dir)? > self.config.reserve_bytes,
            "缓存卷剩余空间低于安全余量，请清理空间或更改缓存位置");
        ensure!(store::directory_size(&self.config.cache_dir) < self.config.cache_limit_bytes, "应用缓存达到总配额，请清理闲置缓存");
        let result_bytes: u64 = fs::read_dir(&self.instance_dir)?.filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("query-") || e.file_name().to_string_lossy().starts_with("candidate-"))
            .map(|e| store::directory_size(&e.path())).sum();
        ensure!(result_bytes <= self.config.result_limit_bytes, "全部查询结果达到全局磁盘配额，请关闭后台标签或减少显示列");
        Ok(())
    }
    pub fn snapshots(&self) -> Vec<SessionSnapshot> {
        self.check_sources();
        self.sessions.lock().unwrap().values().map(|s| s.data.lock().unwrap().snapshot.clone()).collect()
    }
    /// Watchdog snapshots must not wait for a lock held by the work being diagnosed.
    /// They intentionally omit CSV values, previews and filter text.
    pub fn diagnostic_status(&self) -> serde_json::Value {
        let sessions = match self.sessions.try_lock() {
            Ok(sessions) => serde_json::json!(sessions.iter().map(|(id, session)| {
                match session.data.try_lock() {
                    Ok(data) => {
                        let snapshot = &data.snapshot;
                        serde_json::json!({
                            "tabId": id, "status": snapshot.status, "phase": snapshot.phase,
                            "bytesRead": snapshot.bytes_read, "totalBytes": snapshot.total_bytes,
                            "rowsRead": snapshot.rows_read, "columns": snapshot.columns.len(),
                            "revision": snapshot.revision, "cacheHit": snapshot.cache_hit,
                            "error": snapshot.error, "query": snapshot.query,
                        })
                    }
                    Err(_) => serde_json::json!({ "tabId": id, "lockBusy": "session.data" }),
                }
            }).collect::<Vec<_>>()),
            Err(_) => serde_json::json!({ "lockBusy": "sessions" }),
        };
        let waiting = match self.waiting.try_lock() {
            Ok(waiting) => serde_json::json!(waiting.iter().map(|(ticket, (owner, priority))|
                serde_json::json!({ "ticket": ticket, "tabId": owner, "priority": priority })).collect::<Vec<_>>()),
            Err(_) => serde_json::json!({ "lockBusy": "waiting" }),
        };
        serde_json::json!({ "sessions": sessions, "waiting": waiting, "heavySlotBusy": self.heavy.try_lock().is_err() })
    }
    pub fn snapshot(&self, tab: &str) -> Result<SessionSnapshot> { Ok(self.session(tab)?.data.lock().unwrap().snapshot.clone()) }
    pub fn open(self: &Arc<Self>, path: PathBuf, spec: ParseSpec) -> Result<String> {
        ensure!(path.is_file(), "请选择存在的本地文件");
        let path = fs::canonicalize(path)?;
        let tab = id();
        let task = Task::new(&tab, 2);
        let snapshot = SessionSnapshot {
            id: tab.clone(), name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            total_bytes: fs::metadata(&path)?.len().to_string(), path: path.clone(),
            status: "opening".into(), phase: "读取文件配置".into(), bytes_read: "0".into(), rows_read: "0".into(),
            columns: vec![], preview: vec![], parse_spec: spec.clone(), warnings: vec![], error: None,
            revision: 0, view: ViewState::default(), can_undo: false, can_redo: false, query: None, cache_hit: false, modified: false,
        };
        let session = Arc::new(Session {
            data: Mutex::new(SessionData { snapshot, history: History::default(), dataset: None, query: None, baseline: EditState::default() }),
            prepare: Mutex::new(task.clone()),
        });
        {
            let mut sessions = self.sessions.lock().unwrap();
            ensure!(sessions.len() < self.config.max_sessions,
                "当前最多同时打开 {} 个文件，请关闭部分标签后再打开", self.config.max_sessions);
            sessions.insert(tab.clone(), session.clone());
        }
        *self.active.lock().unwrap() = Some(tab.clone());
        self.spawn_prepare(tab.clone(), session, task, path, spec);
        Ok(tab)
    }
    fn spawn_prepare(self: &Arc<Self>, tab: String, session: Arc<Session>, task: Arc<Task>, path: PathBuf, spec: ParseSpec) {
        let engine = self.clone();
        thread::spawn(move || {
            let mut operation = Operation::start("task.prepare",
                serde_json::json!({ "tabId": tab, "path": path }), false);
            let result = engine.prepare(&session, &task, &path, spec);
            operation.finish(result.as_ref().err().map(|error| format!("{error:#}")));
            if let Err(error) = result {
                let mut data = session.data.lock().unwrap();
                if data.snapshot.status != "closed" {
                    data.snapshot.status = if task.cancel.load(Ordering::Relaxed) { "cancelled" } else { "failed" }.into();
                    data.snapshot.phase = data.snapshot.status.clone();
                    data.snapshot.error = Some(format!("{error:#}"));
                }
            } else if engine.session(&tab).is_ok() {
                let columns = session.data.lock().unwrap().snapshot.columns.len().min(12);
                if let Err(error) = engine.start_query(&tab, (0..columns).collect()) {
                    diagnostics::record("error", "initial_query_failed",
                        serde_json::json!({ "tabId": tab, "error": format!("{error:#}") }));
                }
            }
        });
    }
    fn prepare(&self, session: &Session, task: &Task, path: &Path, spec: ParseSpec) -> Result<()> {
        let source_size = fs::metadata(path)?.len();
        let source_modified = modified(path)?;
        let (mut stream, warnings) = CsvStream::open(path, spec)?;
        let spec = stream.spec().clone();
        let first = loop {
            match stream.next(Some(PREVIEW_BYTES as u64), &task.cancel)? {
                ReadOutcome::Record(record) => break record,
                ReadOutcome::Yield => {
                    let mut data = session.data.lock().unwrap();
                    data.snapshot.status = "previewing".into();
                    data.snapshot.phase = "首条记录较长，继续读取".into();
                    data.snapshot.bytes_read = stream.bytes_read.to_string();
                }
                ReadOutcome::End => bail!("文件没有可解析的记录"),
            }
        };
        let width = first.fields.len();
        ensure!(width > 0 && width <= spec.max_columns, "列数无效或超过预算");
        let headers = if spec.has_headers { first.fields.clone() } else { (0..width).map(|i| format!("列 {}", i + 1)).collect() };
        let mut seen = BTreeMap::<String, usize>::new();
        let columns: Vec<_> = headers.into_iter().enumerate().map(|(i, original)| {
            let display = if original.is_empty() { format!("列 {}", i + 1) } else { original.clone() };
            let n = seen.entry(display.clone()).or_default();
            *n += 1;
            Column { id: i, name: if *n == 1 { display } else { format!("{display} [{}]", *n) }, original_name: original }
        }).collect();
        let mut preview = vec![];
        if !spec.has_headers { preview.push(first.fields); }
        let mut eof = false;
        let preview_start = stream.consumed;
        while preview.len() < 200 && stream.consumed - preview_start < PREVIEW_BYTES as u64 {
            match stream.next(Some((PREVIEW_BYTES as u64).saturating_sub(stream.consumed - preview_start)), &task.cancel)? {
                ReadOutcome::Record(record) => {
                    validate_width(&record, width, preview.len() as u64 + 1)?;
                    preview.push(record.fields);
                }
                ReadOutcome::Yield => break,
                ReadOutcome::End => { eof = true; break; }
            }
        }
        {
            let mut data = session.data.lock().unwrap();
            data.snapshot.status = "preparing".into();
            data.snapshot.phase = "准备完整数据".into();
            data.snapshot.columns = columns.clone();
            data.snapshot.parse_spec = spec.clone();
            data.snapshot.preview = preview.iter().map(|r| r.iter().map(|s| s.chars().take(1024).collect()).collect()).collect();
            data.snapshot.bytes_read = stream.bytes_read.to_string();
            data.snapshot.rows_read = preview.len().to_string();
            data.snapshot.warnings = warnings;
        }
        let candidate = fs::read_dir(self.config.cache_dir.join("datasets"))?.filter_map(|e| e.ok()).find_map(|entry| {
            let manifest: DatasetManifest = serde_json::from_slice(&fs::read(entry.path().join("manifest.json")).ok()?).ok()?;
            (manifest.version == FORMAT_VERSION && manifest.engine == "2.0.0"
                && manifest.path == path && manifest.source_size == source_size
                && manifest.source_modified == source_modified && manifest.spec == spec)
                .then_some((entry.path(), manifest))
        });
        if let Some((directory, manifest)) = candidate {
            session.data.lock().unwrap().snapshot.phase = "验证完整内容指纹".into();
            let permit = self.permit(task)?;
            let hash = stream.finish_hash(&task.cancel, |bytes| {
                session.data.lock().unwrap().snapshot.bytes_read = bytes.to_string();
            })?;
            drop(permit);
            if hash == manifest.hash {
                ensure!(modified(path)? == source_modified && fs::metadata(path)?.len() == source_size, "验证期间源文件发生变化");
                return self.attach_ready(session, directory, manifest, true);
            }
            // Strong verification detected a change even though metadata matched.
            let (fresh, _) = CsvStream::open(path, spec.clone())?;
            stream = fresh;
            if spec.has_headers {
                loop { if matches!(stream.next(None, &task.cancel)?, ReadOutcome::Record(_)) { break; } }
            }
            preview.clear();
            eof = false;
        }
        task.check()?;
        self.disk_check()?;
        let directory = self.instance_dir.join(format!("import-{}", id()));
        fs::create_dir_all(&directory)?;
        let alias = format!("d{}", id());
        let conn = self.connection()?;
        conn.execute_batch(&format!("ATTACH {} AS {alias}; CREATE TABLE {alias}.data (_source_rid UBIGINT NOT NULL, {});",
            quote(&directory.join("data.duckdb").to_string_lossy()),
            (0..width).map(|c| format!("c{c} VARCHAR NOT NULL")).collect::<Vec<_>>().join(", ")))?;
        let import_result = (|| -> Result<DatasetManifest> {
            let mut count = 0u64;
            let mut ranges = vec![];
            let mut batch = preview;
            let mut bytes = batch.iter().flatten().map(|s| s.len()).sum::<usize>();
            loop {
                if !batch.is_empty() && (bytes >= 8 * 1024 * 1024 || batch.len() >= 32768 || eof) {
                    let _permit = self.permit(task)?;
                    task.bind(&conn);
                    let first = count + 1;
                    conn.execute_batch("BEGIN")?;
                    {
                        let rows = batch.len();
                        let mut builders: Vec<_> = (0..width).map(|_| StringBuilder::with_capacity(rows, bytes / width)).collect();
                        for (index, row) in batch.drain(..).enumerate() {
                            if index % 128 == 0 { task.check()?; }
                            for (builder, value) in builders.iter_mut().zip(row) { builder.append_value(value); }
                        }
                        let mut arrays: Vec<ArrayRef> = vec![Arc::new(UInt64Array::from_iter_values(first..first + rows as u64))];
                        arrays.extend(builders.iter_mut().map(|b| Arc::new(b.finish()) as ArrayRef));
                        let mut fields = vec![Field::new("_source_rid", DataType::UInt64, false)];
                        fields.extend((0..width).map(|c| Field::new(format!("c{c}"), DataType::Utf8, false)));
                        let records = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)?;
                        let mut appender = conn.appender_to_catalog_and_db("data", &alias, "main")?;
                        appender.append_record_batch(records)?;
                        appender.flush()?;
                        count += rows as u64;
                    }
                    conn.execute_batch("COMMIT")?;
                    ranges.push((first, count));
                    bytes = 0;
                    self.disk_check()?;
                    let mut data = session.data.lock().unwrap();
                    data.snapshot.rows_read = count.to_string();
                    data.snapshot.bytes_read = stream.bytes_read.to_string();
                }
                if eof { break; }
                match stream.next(None, &task.cancel)? {
                    ReadOutcome::Record(record) => {
                        validate_width(&record, width, count + batch.len() as u64 + 1)?;
                        bytes += record.fields.iter().map(|s| s.len()).sum::<usize>();
                        batch.push(record.fields);
                    }
                    ReadOutcome::Yield => {}
                    ReadOutcome::End => eof = true,
                }
            }
            ensure!(modified(path)? == source_modified && fs::metadata(path)?.len() == source_size, "准备期间源文件发生变化");
            let _permit = self.permit(task)?;
            conn.execute_batch(&format!("CHECKPOINT {alias}"))?;
            Ok(DatasetManifest { version: FORMAT_VERSION, engine: "2.0.0".into(), path: path.to_path_buf(),
                source_size, source_modified, hash: stream.hash(), spec, columns, rows: count, ranges,
                last_used: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() })
        })();
        let _ = conn.execute_batch("ROLLBACK");
        let _ = conn.execute_batch(&format!("DETACH {alias}"));
        let manifest = match import_result {
            Ok(m) => m,
            Err(e) => { let _ = fs::remove_dir_all(&directory); return Err(e); }
        };
        task.check()?;
        let fingerprint = blake3::hash(&serde_json::to_vec(&(FORMAT_VERSION, &manifest.engine, &manifest.path, manifest.source_size, manifest.source_modified, &manifest.hash, &manifest.spec))?).to_hex().to_string();
        let target = self.config.cache_dir.join("datasets").join(fingerprint);
        let publication_lease = self.dataset_lease(&target)?;
        fs2::FileExt::lock_shared(&publication_lease)?;
        store::write_new_json(&directory.join("manifest.json"), &manifest)?;
        if target.exists() { fs::remove_dir_all(&directory)?; }
        else { fs::rename(&directory, &target)?; }
        self.attach_ready(session, target, manifest, false)
    }
    fn attach_ready(&self, session: &Session, directory: PathBuf, manifest: DatasetManifest, cache_hit: bool) -> Result<()> {
        let attachment = {
            let mut attachments = self.attachments.lock().unwrap();
            if let Some(existing) = attachments.get(&directory).and_then(Weak::upgrade) {
                existing
            } else {
                let lease = self.dataset_lease(&directory)?;
                fs2::FileExt::lock_shared(&lease)?;
                let alias = format!("d{}", id());
                self.connection.lock().unwrap().execute_batch(&format!("ATTACH {} AS {alias} (READ_ONLY)", quote(&directory.join("data.duckdb").to_string_lossy())))?;
                let attachment = Arc::new(Attachment { alias, connection: Arc::downgrade(&self.connection), _lease: lease });
                attachments.insert(directory.clone(), Arc::downgrade(&attachment));
                attachment
            }
        };
        let _ = filetime_touch(&directory.join("manifest.json"));
        let mut data = session.data.lock().unwrap();
        ensure!(!matches!(data.snapshot.status.as_str(), "closed" | "cancelling" | "cancelled"), "任务已取消");
        data.snapshot.status = "ready".into();
        data.snapshot.phase = if cache_hit { "已验证并复用缓存" } else { "完整数据已就绪" }.into();
        data.snapshot.bytes_read = manifest.source_size.to_string();
        data.snapshot.rows_read = manifest.rows.to_string();
        data.snapshot.columns = manifest.columns.clone();
        data.snapshot.preview.clear();
        data.snapshot.cache_hit = cache_hit;
        let metadata_path = color_path(&manifest.path);
        if metadata_path.exists() {
            let loaded = (|| -> Result<EditState> {
                ensure!(fs::metadata(&metadata_path)?.len() <= 16 * 1024 * 1024, "颜色标记文件超过 16MiB");
                let metadata: ColorMetadata = serde_json::from_slice(&fs::read(&metadata_path)?)?;
                ensure!(metadata.version == 1 && metadata.csv_hash == manifest.hash
                    && metadata.has_headers == manifest.spec.has_headers && metadata.delimiter == manifest.spec.delimiter,
                    "颜色标记与当前 CSV 内容或解析设置不匹配");
                let edits = EditState { colors: metadata.colors, ..Default::default() };
                editing::validate(&edits, manifest.columns.len(), manifest.rows, manifest.spec.max_field_bytes)?;
                Ok(edits)
            })();
            match loaded {
                Ok(edits) => {
                    data.history.load_colors(&edits);
                    data.snapshot.view = (*data.history.present).clone();
                    data.baseline = edits;
                }
                Err(error) => data.snapshot.warnings.push(format!("未加载颜色标记：{error}")),
            }
        }
        diagnostics::record("info", "dataset_ready", serde_json::json!({
            "tabId": data.snapshot.id, "rows": manifest.rows,
            "columns": manifest.columns.len(), "cacheHit": cache_hit,
        }));
        data.dataset = Some(Dataset { alias: attachment.alias.clone(), manifest, _attachment: attachment });
        Ok(())
    }
    fn dataset_lease(&self, directory: &Path) -> Result<File> {
        let name = directory.file_name().context("缓存目录无效")?.to_string_lossy();
        Ok(OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .open(self.config.cache_dir.join("leases").join(format!("{name}.lock")))?)
    }
    fn resolve(conn: &Connection, dataset: &Dataset, view: &ViewState, value: &FilterValue) -> Result<String> {
        match value {
            FilterValue::Text { text } => Ok(text.clone()),
            FilterValue::Reference { rid, column, digest, byte_length } => {
                let c = column_name(*column, dataset.manifest.columns.len())?;
                let rid: u64 = rid.parse()?;
                let text: String = if let Some(value) = view.edits.cells.get(&rid).and_then(|cells| cells.get(column)) {
                    value.clone()
                } else {
                    conn.query_row(&format!("SELECT {c} FROM {}.data WHERE _source_rid = ?", dataset.alias), [rid], |row| row.get(0))?
                };
                if !digest.is_empty() {
                    ensure!(text.len() as u64 == *byte_length && blake3::hash(text.as_bytes()).to_hex().as_str() == digest,
                        "值引用与文件版本不匹配，请重新打开筛选菜单");
                }
                Ok(text)
            }
        }
    }
    pub fn commit(self: &Arc<Self>, tab: &str, base_revision: u64, view: ViewState) -> Result<SessionSnapshot> {
        ensure!(serde_json::to_vec(&(&view.filters, &view.sort))?.len() <= 4 * 1024 * 1024, "筛选状态超过 4MiB");
        ensure!(serde_json::to_vec(&view.edits)?.len() <= 16 * 1024 * 1024, "编辑和颜色状态超过 16MiB，请先另存为 CSV 后继续编辑");
        let session = self.session(tab)?;
        let should_query;
        let projection;
        {
            let mut data = session.data.lock().unwrap();
            ensure!(data.snapshot.revision == base_revision, "状态已更新，请刷新后重试");
            ensure!(!matches!(data.snapshot.status.as_str(), "closed" | "stale"), "文件已关闭或源发生变化");
            // Compile before committing to catch invalid columns and bound semantics.
            let width = data.snapshot.columns.len();
            if view.edits != data.snapshot.view.edits {
                let dataset = data.dataset.as_ref().context("完整数据就绪后才能编辑")?;
                editing::validate(&view.edits, width, dataset.manifest.rows, data.snapshot.parse_spec.max_field_bytes)?;
            }
            ensure!(view.filters.keys().chain(view.sort.iter().map(|s| &s.column))
                .all(|column| !view.edits.deleted_columns.contains(column)), "已删除的列不能用于筛选或排序");
            compile_filters(&view, None, width, |v| match v {
                FilterValue::Text { text } => Ok(text.clone()),
                _ => Ok(String::new()),
            })?;
            compile_order(&view, width)?;
            if !data.history.commit(view) { return Ok(data.snapshot.clone()); }
            data.snapshot.view = (*data.history.present).clone();
            data.snapshot.modified = data.snapshot.view.edits != data.baseline;
            data.snapshot.revision += 1;
            data.snapshot.can_undo = data.history.can_undo();
            data.snapshot.can_redo = data.history.can_redo();
            projection = data.snapshot.query.as_ref().map(|q| q.projection.clone()).unwrap_or_else(|| (0..width.min(12)).collect());
            should_query = data.dataset.is_some();
        }
        self.cancel_candidates(tab);
        self.trim_histories();
        if should_query { self.start_query(tab, projection)?; }
        self.snapshot(tab)
    }
    pub fn history(self: &Arc<Self>, tab: &str, base_revision: u64, redo: bool) -> Result<SessionSnapshot> {
        let session = self.session(tab)?;
        let columns;
        let ready;
        {
            let mut data = session.data.lock().unwrap();
            ensure!(data.snapshot.revision == base_revision, "状态已更新，请刷新");
            let changed = if redo { data.history.redo() } else { data.history.undo() };
            if !changed { return Ok(data.snapshot.clone()); }
            data.snapshot.view = (*data.history.present).clone();
            data.snapshot.modified = data.snapshot.view.edits != data.baseline;
            data.snapshot.revision += 1;
            data.snapshot.can_undo = data.history.can_undo();
            data.snapshot.can_redo = data.history.can_redo();
            columns = data.snapshot.query.as_ref().map(|q| q.projection.clone()).unwrap_or_default();
            ready = data.dataset.is_some();
        }
        self.cancel_candidates(tab);
        if ready { self.start_query(tab, columns)?; }
        self.snapshot(tab)
    }
    pub fn start_query(self: &Arc<Self>, tab: &str, mut projection: Vec<usize>) -> Result<String> {
        let session = self.session(tab)?;
        let (dataset, view, revision, previous) = {
            let data = session.data.lock().unwrap();
            ensure!(data.snapshot.status == "ready", "完整数据尚未就绪");
            (data.dataset.clone().context("数据未就绪")?, data.snapshot.view.clone(), data.snapshot.revision, data.query.clone())
        };
        projection.sort_unstable();
        projection.dedup();
        ensure!(!projection.is_empty() && projection.len() <= 128, "一次投影需包含 1–128 个可见列");
        for &c in &projection { column_name(c, dataset.manifest.columns.len())?; }
        let query_id = id();
        let task = Task::new(tab, 1);
        let direct = view.filters.is_empty() && view.sort.is_empty() && view.edits.deleted_rows.is_empty();
        if let Some(old) = previous.as_ref().filter(|q| q.store.complete()
            && q.view.filters == view.filters && q.view.sort == view.sort && q.view.edits.same_data(&view.edits)
            && projection.iter().all(|c| q.store.columns.contains(c))) {
            let query = Arc::new(Query { id: query_id.clone(), task, store: old.store.clone(), view, revision, direct: old.direct });
            let mut data = session.data.lock().unwrap();
            ensure!(data.snapshot.revision == revision, "Stale：查询上下文已过期");
            data.query = Some(query);
            data.snapshot.query = Some(QuerySnapshot {
                id: query_id.clone(), revision, status: "ready".into(),
                generated_rows: old.store.generated().to_string(), exact_total: Some(old.store.generated().to_string()),
                projection, error: None,
            });
            return Ok(query_id);
        }
        let previous = previous.filter(|q| q.revision == revision && q.view == view);
        if let Some(q) = &previous {
            let status = session.data.lock().unwrap().snapshot.query.as_ref().map(|s| s.status.clone()).unwrap_or_default();
            if !matches!(status.as_str(), "failed" | "cancelled") {
                if projection.iter().all(|c| q.store.columns.contains(c)) || !q.store.complete() {
                    if q.direct {
                        if let Some(snapshot) = session.data.lock().unwrap().snapshot.query.as_mut() {
                            snapshot.projection = projection;
                        }
                    }
                    return Ok(q.id.clone());
                }
            }
        }
        let source = previous.filter(|q| q.store.complete()).map(|q| q.store.clone());
        let exact_total = if direct {
            Some(dataset.manifest.rows)
        } else {
            source.as_ref().map(|store| store.generated())
        };
        let store_columns = if direct { (0..dataset.manifest.columns.len()).collect() } else { projection.clone() };
        let store = ResultStore::create(self.instance_dir.join(format!("query-{query_id}")), store_columns)?;
        if direct { store.identity(dataset.manifest.rows); }
        let query = Arc::new(Query { id: query_id.clone(), task: task.clone(), store: store.clone(), view: view.clone(), revision, direct });
        {
            let mut data = session.data.lock().unwrap();
            ensure!(data.snapshot.revision == revision && data.snapshot.status == "ready", "Stale：查询上下文已过期");
            if let Some(old) = data.query.take() { old.task.stop(); }
            data.query = Some(query.clone());
            data.snapshot.query = Some(QuerySnapshot {
                id: query_id.clone(), revision, status: if direct { "ready" } else { "queued" }.into(),
                generated_rows: if direct { dataset.manifest.rows.to_string() } else { "0".into() },
                exact_total: exact_total.map(|total| total.to_string()),
                projection: projection.clone(), error: None,
            });
        }
        if direct {
            diagnostics::record("info", "query_ready", serde_json::json!({
                "tabId": tab, "queryId": query_id, "rows": dataset.manifest.rows, "direct": true,
            }));
            return Ok(query_id);
        }
        let engine = self.clone();
        thread::spawn(move || {
            let mut operation = Operation::start("task.query", serde_json::json!({
                "queryId": query.id, "tabId": task.owner, "revision": revision,
                "columns": projection, "filterColumns": view.filters.len(), "sortKeys": view.sort.len(),
                "projectionOnly": source.is_some(),
            }), false);
            let result = if let Some(source) = source {
                engine.build_projection(&session, &dataset, &query, &source)
            } else {
                engine.build_query(&session, &dataset, &query)
            };
            operation.finish(result.as_ref().err().map(|error| format!("{error:#}")));
            let mut data = session.data.lock().unwrap();
            if let Some(snapshot) = data.snapshot.query.as_mut().filter(|q| q.id == query.id) {
                match result {
                    Ok(rows) => { snapshot.status = "ready".into(); snapshot.exact_total = Some(rows.to_string()); snapshot.generated_rows = rows.to_string(); }
                    Err(error) => {
                        snapshot.status = if task.cancel.load(Ordering::Relaxed) { "cancelled" } else { "failed" }.into();
                        snapshot.error = Some(format!("{error:#}"));
                    }
                }
            }
        });
        Ok(query_id)
    }
    fn build_projection(&self, session: &Session, dataset: &Dataset, query: &Query, source: &ResultStore) -> Result<u64> {
        let _permit = self.permit(&query.task)?;
        let conn = self.connection()?;
        query.task.bind(&conn);
        self.update_query_progress(session, &query.id, 0);
        // This temporary relation contains identities only. It never repeats WHERE or user sorting.
        conn.execute_batch("CREATE TEMP TABLE projection_positions (position UBIGINT, rid UBIGINT)")?;
        let mut input = BufReader::with_capacity(1024 * 1024, File::open(source.rid_file())?);
        {
            let mut appender = conn.appender("projection_positions")?;
            for start in (0..source.generated()).step_by(32768) {
                query.task.check()?;
                let end = (start + 32768).min(source.generated());
                let mut ids = Vec::with_capacity((end - start) as usize);
                for _ in start..end {
                    let mut bytes = [0; 8];
                    input.read_exact(&mut bytes)?;
                    ids.push(u64::from_le_bytes(bytes));
                }
                let schema = Schema::new(vec![
                    Field::new("position", DataType::UInt64, false),
                    Field::new("rid", DataType::UInt64, false),
                ]);
                let batch = RecordBatch::try_new(Arc::new(schema), vec![
                    Arc::new(UInt64Array::from_iter_values(start..end)),
                    Arc::new(UInt64Array::from(ids)),
                ])?;
                appender.append_record_batch(batch)?;
            }
            appender.flush()?;
        }
        let selected = query.store.columns.iter().map(|c| format!("d.c{c}")).collect::<Vec<_>>().join(", ");
        let mut statement = conn.prepare(&format!(
            "SELECT d._source_rid, {selected} FROM projection_positions p JOIN {} d ON d._source_rid=p.rid ORDER BY p.position",
            editing::source(&dataset.alias, dataset.manifest.columns.len(), &query.view.edits)))?;
        let mut writer = query.store.writer(self.config.result_limit_bytes)?;
        for batch in statement.stream_arrow([])? {
            query.task.check()?;
            if writer.append(&batch?)? {
                self.update_query_progress(session, &query.id, query.store.generated());
                self.disk_check()?;
            }
        }
        let total = writer.finish()?;
        ensure!(total == source.generated(), "投影记录数量不一致");
        Ok(total)
    }
    fn build_query(&self, session: &Session, dataset: &Dataset, query: &Query) -> Result<u64> {
        let _permit = self.permit(&query.task)?;
        self.disk_check()?;
        let conn = self.connection()?;
        query.task.bind(&conn);
        let filters = compile_filters(&query.view, None, dataset.manifest.columns.len(), |v| Self::resolve(&conn, dataset, &query.view, v))?;
        let columns = query.store.columns.iter().map(|c| format!("c{c}")).collect::<Vec<_>>().join(", ");
        let mut writer = query.store.writer(self.config.result_limit_bytes)?;
        self.update_query_progress(session, &query.id, 0);
        let ranges = if query.view.sort.is_empty() { dataset.manifest.ranges.clone() } else { vec![(1, dataset.manifest.rows)] };
        for (lo, hi) in ranges {
            query.task.check()?;
            let order = compile_order(&query.view, dataset.manifest.columns.len())?;
            let sql = format!("SELECT _source_rid, {columns} FROM {} WHERE ({}) AND _source_rid BETWEEN {lo} AND {hi} ORDER BY {order}",
                editing::source(&dataset.alias, dataset.manifest.columns.len(), &query.view.edits), filters.sql);
            let mut statement = conn.prepare(&sql)?;
            let batches = statement.stream_arrow(params_from_iter(filters.parameters.iter()))?;
            for batch in batches {
                query.task.check()?;
                let batch = batch?;
                if writer.append(&batch)? {
                    self.update_query_progress(session, &query.id, query.store.generated());
                    self.disk_check()?;
                }
            }
        }
        writer.finish()
    }
    fn update_query_progress(&self, session: &Session, id: &str, generated: u64) {
        let mut data = session.data.lock().unwrap();
        if let Some(query) = data.snapshot.query.as_mut().filter(|q| q.id == id) {
            query.status = "building".into();
            query.generated_rows = generated.to_string();
        }
    }
    pub fn page(&self, tab: &str, query_id: &str, start: u64, count: usize, columns: Vec<usize>) -> Result<Vec<u8>> {
        let session = self.session(tab)?;
        let query = {
            let data = session.data.lock().unwrap();
            ensure!(data.snapshot.status == "ready", "源数据已变化或尚未就绪");
            let query = data.query.clone().context("尚无查询")?;
            ensure!(data.snapshot.revision == query.revision, "Stale：查询版本已过期");
            query
        };
        ensure!(query.id == query_id, "Stale：查询已过期");
        ensure!(columns.iter().all(|c| query.store.columns.contains(c)), "ProjectionPending：需要加载这些列");
        let batch = self.raw_page(&session, &query, start, count.min(512), &columns, true)?;
        store::to_ipc(&store::display_batch(&batch, &columns)?)
    }
    fn raw_page(&self, session: &Session, query: &Query, start: u64, count: usize, columns: &[usize], truncate: bool) -> Result<RecordBatch> {
        ensure!(count > 0 && count <= 2048, "无效页大小");
        if !query.direct { return query.store.read(start, count); }
        let dataset = session.data.lock().unwrap().dataset.clone().context("数据未就绪")?;
        ensure!(start < dataset.manifest.rows, "结果位置越界");
        let conn = self.connection()?;
        let selected = columns.iter().map(|&c| {
            let name = column_name(c, dataset.manifest.columns.len())?;
            Ok(if truncate { format!("left({name}, 1024) AS {name}") } else { name })
        }).collect::<Result<Vec<_>>>()?.join(", ");
        let selected = if selected.is_empty() { String::new() } else { format!(", {selected}") };
        let mut statement = conn.prepare(&format!(
            "SELECT _source_rid{selected} FROM {} WHERE _source_rid BETWEEN {} AND {} ORDER BY _source_rid",
            editing::source(&dataset.alias, dataset.manifest.columns.len(), &query.view.edits), start + 1, start + count as u64))?;
        let mut batches = vec![];
        for batch in statement.stream_arrow([])? { batches.push(batch?); }
        let schema = batches.first().context("没有数据")?.schema();
        Ok(duckdb::arrow::compute::concat_batches(&schema, &batches)?)
    }
    pub fn copy_cells(&self, tab: &str, query_id: &str, start: u64, count: usize, columns: Vec<usize>, include_rid: bool) -> Result<Vec<Vec<String>>> {
        ensure!(count.saturating_mul(columns.len() + usize::from(include_rid)) <= 100_000, "复制超过 100,000 单元格，请使用导出");
        let session = self.session(tab)?;
        let query = session.data.lock().unwrap().query.clone().context("没有查询")?;
        ensure!(query.id == query_id, "查询已过期");
        let mut result = vec![];
        let mut bytes = 0;
        for offset in (0..count).step_by(512) {
            let batch = self.raw_page(&session, &query, start + offset as u64, (count - offset).min(512), &columns, false)?;
            for row in 0..batch.num_rows() {
                let mut values = vec![];
                if include_rid { values.push(array_value_to_string(batch.column(0).as_ref(), row)?); }
                for column in &columns {
                    let index = batch.schema().index_of(&format!("c{column}"))?;
                    values.push(array_value_to_string(batch.column(index).as_ref(), row)?);
                }
                bytes += values.iter().map(|s| s.len()).sum::<usize>();
                ensure!(bytes <= 8 * 1024 * 1024, "复制超过 8MiB，请使用导出");
                result.push(values);
            }
        }
        Ok(result)
    }
    pub fn full_cell(&self, tab: &str, rid: u64, column: usize) -> Result<String> {
        let session = self.session(tab)?;
        let (dataset, view) = {
            let data = session.data.lock().unwrap();
            (data.dataset.clone().context("数据尚未就绪")?, data.snapshot.view.clone())
        };
        Self::resolve(&self.connection()?, &dataset, &view, &FilterValue::Reference { rid: rid.to_string(), column, digest: String::new(), byte_length: 0 })
    }
    pub fn candidates(self: &Arc<Self>, tab: &str, revision: u64, column: usize, search: String) -> Result<String> {
        self.candidates_with_rules(tab, revision, column, search, vec![], vec![])
    }
    pub fn candidates_with_rules(self: &Arc<Self>, tab: &str, revision: u64, column: usize, search: String, searches: Vec<String>, selected_values: Vec<FilterValue>) -> Result<String> {
        ensure!(searches.len() <= 64, "选择规则超过上限");
        ensure!(selected_values.len() <= 100, "一次最多检查 100 个显式值的可用状态");
        let session = self.session(tab)?;
        let (dataset, view) = {
            let data = session.data.lock().unwrap();
            ensure!(data.snapshot.revision == revision && data.snapshot.status == "ready", "候选上下文已过期或数据未就绪");
            (data.dataset.clone().unwrap(), data.snapshot.view.clone())
        };
        column_name(column, dataset.manifest.columns.len())?;
        self.cancel_candidates(tab);
        let candidate_id = id();
        let candidate = Arc::new(CandidateTask {
            snapshot: Mutex::new(CandidateSnapshot { id: candidate_id.clone(), tab_id: tab.into(), revision, column, search: search.clone(),
                status: "queued".into(), count: "0".into(), error: None, availability: vec![] }),
            task: Task::new(tab, 0),
            store: ResultStore::create(self.instance_dir.join(format!("candidate-{candidate_id}")), vec![column])?,
            searches,
        });
        self.candidates.lock().unwrap().insert(candidate_id.clone(), candidate.clone());
        let engine = self.clone();
        thread::spawn(move || {
            let mut operation = Operation::start("task.candidates", serde_json::json!({
                "candidateId": candidate.snapshot.lock().unwrap().id,
                "tabId": candidate.task.owner, "column": column, "revision": revision,
                "searchBytes": search.len(), "selectedValues": selected_values.len(),
            }), false);
            let result = (|| -> Result<u64> {
                let _permit = engine.permit(&candidate.task)?;
                let conn = engine.connection()?;
                candidate.task.bind(&conn);
                candidate.snapshot.lock().unwrap().status = "building".into();
                let mut compiled = compile_filters(&view, Some(column), dataset.manifest.columns.len(), |v| Self::resolve(&conn, &dataset, &view, v))?;
                let source = editing::source(&dataset.alias, dataset.manifest.columns.len(), &view.edits);
                let sql = format!("SELECT min(_source_rid)::UBIGINT AS _source_rid, c{column} FROM {source}
                    WHERE ({}) AND contains(c{column}, ?) GROUP BY c{column} ORDER BY c{column}", compiled.sql);
                compiled.parameters.push(search);
                let mut statement = conn.prepare(&sql)?;
                let mut writer = candidate.store.writer(1024 * 1024 * 1024)?;
                for batch in statement.stream_arrow(params_from_iter(compiled.parameters.iter()))? {
                    candidate.task.check()?;
                    if writer.append(&batch?)? {
                        candidate.snapshot.lock().unwrap().count = candidate.store.generated().to_string();
                        engine.disk_check()?;
                    }
                }
                let total = writer.finish()?;
                if !selected_values.is_empty() {
                    candidate.task.check()?;
                    let mut points = vec![];
                    let mut bytes = 0usize;
                    for value in selected_values {
                        let text = Self::resolve(&conn, &dataset, &view, &value)?;
                        bytes = bytes.saturating_add(text.len());
                        points.push((value, if bytes <= 4 * 1024 * 1024 { Some(text) } else { None }));
                    }
                    let mut availability = compile_filters(&view, Some(column), dataset.manifest.columns.len(), |v| Self::resolve(&conn, &dataset, &view, v))?;
                    let values: Vec<_> = points.iter().filter_map(|(_, s)| s.as_ref()).cloned().collect();
                    let mut present = std::collections::BTreeSet::new();
                    if !values.is_empty() {
                        let placeholders = vec!["?"; values.len()].join(",");
                        let sql = format!("SELECT c{column} FROM {source} WHERE ({}) AND c{column} IN ({placeholders}) GROUP BY c{column}",
                            availability.sql);
                        availability.parameters.extend(values);
                        let mut statement = conn.prepare(&sql)?;
                        let rows = statement.query_map(params_from_iter(availability.parameters.iter()), |r| r.get::<_, String>(0))?;
                        for row in rows { candidate.task.check()?; present.insert(row?); }
                    }
                    candidate.snapshot.lock().unwrap().availability = points.into_iter().map(|(value, text)| ValueAvailability {
                        value, available: text.map(|s| present.contains(&s)),
                    }).collect();
                }
                Ok(total)
            })();
            operation.finish(result.as_ref().err().map(|error| format!("{error:#}")));
            let mut snapshot = candidate.snapshot.lock().unwrap();
            match result {
                Ok(n) => { snapshot.status = "ready".into(); snapshot.count = n.to_string(); }
                Err(error) => {
                    snapshot.status = if candidate.task.cancel.load(Ordering::Relaxed) { "cancelled" } else { "failed" }.into();
                    snapshot.error = Some(format!("{error:#}"));
                }
            }
        });
        Ok(candidate_id)
    }
    pub fn candidate_page(&self, candidate_id: &str, cursor: u64) -> Result<CandidatePage> {
        let candidate = self.candidates.lock().unwrap().get(candidate_id).cloned().context("候选菜单已关闭")?;
        let snapshot = candidate.snapshot.lock().unwrap().clone();
        ensure!(self.snapshot(&snapshot.tab_id)?.revision == snapshot.revision, "Stale：候选上下文已过期");
        let generated = candidate.store.generated();
        let mut items = vec![];
        if cursor < generated {
            let batch = candidate.store.read(cursor, 200)?;
            let ids = batch.column(0).as_any().downcast_ref::<UInt64Array>().context("候选序号无效")?;
            let mut bytes = 0usize;
            for row in 0..batch.num_rows() {
                let text = array_value_to_string(batch.column(1).as_ref(), row)?;
                let label: String = text.chars().take(512).collect();
                if bytes + label.len() > 1024 * 1024 { break; }
                bytes += label.len();
                let value = if text.len() <= 4096 { FilterValue::text(text.clone()) }
                    else { FilterValue::Reference {
                        rid: ids.value(row).to_string(), column: snapshot.column,
                        digest: blake3::hash(text.as_bytes()).to_hex().to_string(), byte_length: text.len() as u64,
                    } };
                let matching_searches = candidate.searches.iter().filter(|s| text.contains(s.as_str())).cloned().collect();
                items.push(Candidate { empty: text.is_empty(), label, value, matching_searches });
            }
        }
        let next_position = cursor + items.len() as u64;
        Ok(CandidatePage { id: snapshot.id, revision: snapshot.revision, items,
            next: (next_position < generated || snapshot.status != "ready").then(|| next_position.to_string()),
            exact_total: (snapshot.status == "ready").then_some(snapshot.count), status: snapshot.status })
    }
    pub fn candidate_snapshot(&self, id: &str) -> Result<CandidateSnapshot> {
        Ok(self.candidates.lock().unwrap().get(id).context("候选已关闭")?.snapshot.lock().unwrap().clone())
    }
    fn cancel_candidates(&self, tab: &str) {
        self.candidates.lock().unwrap().retain(|_, candidate| {
            if candidate.snapshot.lock().unwrap().tab_id != tab { return true; }
            candidate.task.stop();
            false
        });
    }
    pub fn close_candidate(&self, id: &str) {
        if let Some(candidate) = self.candidates.lock().unwrap().remove(id) { candidate.task.stop(); }
    }
    pub fn cancel(&self, tab: &str) -> Result<()> {
        let session = self.session(tab)?;
        session.prepare.lock().unwrap().stop();
        let mut data = session.data.lock().unwrap();
        if matches!(data.snapshot.status.as_str(), "opening" | "previewing" | "preparing") { data.snapshot.status = "cancelling".into(); }
        if let Some(query) = &data.query { query.task.stop(); }
        drop(data);
        self.cancel_candidates(tab);
        for export in self.exports.lock().unwrap().values() {
            if export.snapshot.lock().unwrap().tab_id == tab { export.task.stop(); }
        }
        Ok(())
    }
    pub fn close(&self, tab: &str) -> Result<()> {
        self.cancel(tab)?;
        if let Some(session) = self.sessions.lock().unwrap().remove(tab) {
            session.data.lock().unwrap().snapshot.status = "closed".into();
        }
        Ok(())
    }
    fn check_sources(&self) {
        let tabs: Vec<_> = self.sessions.lock().unwrap().values().cloned().collect();
        for session in tabs {
            let mut data = session.data.lock().unwrap();
            if data.snapshot.status != "ready" { continue; }
            if let Some(dataset) = &data.dataset {
                if modified(&dataset.manifest.path).ok() != Some(dataset.manifest.source_modified)
                    || fs::metadata(&dataset.manifest.path).map(|m| m.len()).ok() != Some(dataset.manifest.source_size) {
                    data.snapshot.status = "stale".into();
                    data.snapshot.phase = "源文件发生变化，请重新打开".into();
                    data.snapshot.revision += 1;
                    if let Some(query) = &data.query { query.task.stop(); }
                }
            }
        }
    }
    fn trim_histories(&self) {
        let sessions: Vec<_> = self.sessions.lock().unwrap().values().cloned().collect();
        let mut bytes: usize = sessions.iter().map(|s| s.data.lock().unwrap().history.bytes()).sum();
        while bytes > 32 * 1024 * 1024 {
            let mut changed = false;
            for session in &sessions {
                let mut data = session.data.lock().unwrap();
                let before = data.history.bytes();
                if data.history.trim_oldest() {
                    bytes -= before.saturating_sub(data.history.bytes());
                    data.snapshot.can_undo = data.history.can_undo();
                    data.snapshot.can_redo = data.history.can_redo();
                    changed = true;
                }
                if bytes <= 32 * 1024 * 1024 { break; }
            }
            if !changed { break; }
        }
    }
    pub fn activate(&self, tab: &str) -> Result<()> {
        *self.active.lock().unwrap() = Some(tab.into());
        let session = self.session(tab)?;
        let mut data = session.data.lock().unwrap();
        if let Some(dataset) = &data.dataset {
            if modified(&dataset.manifest.path).ok() != Some(dataset.manifest.source_modified)
                || fs::metadata(&dataset.manifest.path).map(|m| m.len()).ok() != Some(dataset.manifest.source_size) {
                data.snapshot.status = "stale".into();
                data.snapshot.phase = "源文件发生变化，请重新加载".into();
                if let Some(query) = &data.query { query.task.stop(); }
            }
        }
        Ok(())
    }
    pub fn export(self: &Arc<Self>, tab: &str, query_id: &str, spec: ExportSpec) -> Result<String> {
        ensure!(!spec.path.exists(), "目标已存在，请选择新文件名");
        ensure!(!color_path(&spec.path).exists(), "目标的颜色标记文件已存在，请选择新文件名");
        let session = self.session(tab)?;
        let (dataset, mut query, mut total) = {
            let data = session.data.lock().unwrap();
            ensure!(data.snapshot.status == "ready", "源数据尚未就绪或已变化");
            let q = data.query.clone().context("尚无结果")?;
            ensure!(q.id == query_id && q.store.complete(), "结果尚未完整生成");
            (data.dataset.clone().unwrap(), q.clone(), q.store.generated())
        };
        if spec.all_rows {
            let mut view = query.view.clone();
            view.filters.clear();
            total = dataset.manifest.rows - view.edits.deleted_rows.len() as u64;
            query = Arc::new(Query {
                id: query.id.clone(), task: query.task.clone(), store: query.store.clone(),
                view, revision: query.revision, direct: false,
            });
        }
        for &column in &spec.columns {
            column_name(column, dataset.manifest.columns.len())?;
            ensure!(!query.view.edits.deleted_columns.contains(&column), "已删除的列不能导出");
        }
        ensure!(spec.columns.iter().collect::<std::collections::BTreeSet<_>>().len() == spec.columns.len(), "导出列不能重复");
        ensure!(!spec.columns.is_empty(), "请选择导出列");
        ensure!(spec.delimiter.len() == 1 && matches!(spec.delimiter.as_bytes()[0], b',' | b';' | b'\t' | b'|'), "无效分隔符");
        ensure!(matches!(spec.encoding.as_str(), "utf-8" | "gbk" | "gb18030"), "无效编码");
        if spec.path.exists() { ensure!(!same_file::is_same_file(&dataset.manifest.path, &spec.path)?, "不能覆盖源文件"); }
        let export_id = id();
        let export = Arc::new(ExportTask {
            task: Task::new(tab, 1), snapshot: Mutex::new(ExportSnapshot {
                id: export_id.clone(), tab_id: tab.into(), status: "queued".into(), path: spec.path.clone(),
                rows_written: "0".into(), total_rows: total.to_string(), error: None,
            }),
        });
        self.exports.lock().unwrap().insert(export_id.clone(), export.clone());
        let engine = self.clone();
        thread::spawn(move || {
            let mut operation = Operation::start("task.export", serde_json::json!({
                "tabId": export.task.owner, "exportId": export.snapshot.lock().unwrap().id,
                "path": spec.path, "columns": spec.columns.len(), "rows": total,
            }), false);
            let result = engine.write_export(&dataset, &query, &export, &spec, total);
            operation.finish(result.as_ref().err().map(|error| format!("{error:#}")));
            let mut snapshot = export.snapshot.lock().unwrap();
            match result {
                Ok(()) => snapshot.status = "completed".into(),
                Err(error) => {
                    snapshot.status = if export.task.cancel.load(Ordering::Relaxed) { "cancelled" } else { "failed" }.into();
                    snapshot.error = Some(format!("{error:#}"));
                }
            }
        });
        Ok(export_id)
    }
    fn write_export(&self, dataset: &Dataset, query: &Query, export: &ExportTask, spec: &ExportSpec, total: u64) -> Result<()> {
        let _permit = self.permit(&export.task)?;
        export.snapshot.lock().unwrap().status = "running".into();
        let conn = self.connection()?;
        export.task.bind(&conn);
        let compiled = compile_filters(&query.view, None, dataset.manifest.columns.len(), |v| Self::resolve(&conn, dataset, &query.view, v))?;
        let order = compile_order(&query.view, dataset.manifest.columns.len())?;
        let selected = spec.columns.iter().map(|c| format!("c{c}")).collect::<Vec<_>>().join(", ");
        let mut statement = conn.prepare(&format!("SELECT _source_rid, {selected} FROM {} WHERE {} ORDER BY {order}",
            editing::source(&dataset.alias, dataset.manifest.columns.len(), &query.view.edits), compiled.sql))?;
        let parent = spec.path.parent().context("目标路径无效")?;
        ensure!(parent.is_dir(), "目标目录不存在");
        ensure!(fs2::available_space(parent)? > self.config.reserve_bytes, "导出目标磁盘空间不足");
        let temporary = parent.join(format!(".{}.{}.part", spec.path.file_name().unwrap_or_default().to_string_lossy(), id()));
        let metadata_target = color_path(&spec.path);
        let metadata_temporary = color_path(&temporary);
        let mut published_metadata = false;
        let result = (|| -> Result<()> {
            let mut file = HashingWriter {
                output: BufWriter::with_capacity(1024 * 1024, OpenOptions::new().write(true).create_new(true).open(&temporary)?),
                hasher: blake3::Hasher::new(),
            };
            if spec.bom && spec.encoding == "utf-8" { file.write_all(&[0xef, 0xbb, 0xbf])?; }
            let mut writer = csv::WriterBuilder::new().delimiter(spec.delimiter.as_bytes()[0])
                .terminator(csv::Terminator::CRLF).from_writer(CsvBuffer(std::cell::RefCell::new(Vec::with_capacity(1024 * 1024))));
            if spec.headers { writer.write_record(spec.columns.iter().map(|&c| &dataset.manifest.columns[c].original_name))?; }
            let mut written = 0u64;
            let mut colors = BTreeMap::new();
            for batch in statement.stream_arrow(params_from_iter(compiled.parameters.iter()))? {
                export.task.check()?;
                let batch = batch?;
                for row in 0..batch.num_rows() {
                    export.task.check()?;
                    let rid = batch.column(0).as_any().downcast_ref::<UInt64Array>().context("导出记录序号无效")?.value(row);
                    let fields = batch.columns()[1..].iter().map(|array| array_value_to_string(array.as_ref(), row)).collect::<std::result::Result<Vec<_>, _>>()?;
                    writer.write_record(&fields)?;
                    written += 1;
                    if let Some(marked) = query.view.edits.colors.get(&rid) {
                        let exported: BTreeMap<_, _> = spec.columns.iter().enumerate()
                            .filter_map(|(new_column, original_column)| marked.get(original_column).map(|color| (new_column, color.clone()))).collect();
                        if !exported.is_empty() { colors.insert(written, exported); }
                    }
                    if written % 128 == 0 || writer.get_ref().0.borrow().len() >= 1024 * 1024 {
                        flush_encoded(&mut writer, &mut file, &spec.encoding)?;
                    }
                }
                export.snapshot.lock().unwrap().rows_written = written.to_string();
                ensure!(fs2::available_space(parent)? > self.config.reserve_bytes, "导出过程中目标磁盘空间不足");
            }
            flush_encoded(&mut writer, &mut file, &spec.encoding)?;
            ensure!(written == total, "导出数量与冻结结果不一致");
            file.flush()?;
            file.output.get_ref().sync_all()?;
            let csv_hash = file.hasher.finalize().to_hex().to_string();
            drop(file);
            export.task.check()?;
            if !colors.is_empty() {
                let metadata = ColorMetadata {
                    version: 1, csv_hash, has_headers: spec.headers,
                    delimiter: Some(spec.delimiter.clone()), colors,
                };
                let mut output = OpenOptions::new().write(true).create_new(true).open(&metadata_temporary)?;
                serde_json::to_writer(&mut output, &metadata)?;
                output.sync_all()?;
                drop(output);
                publish_new_file(&metadata_temporary, &metadata_target)?;
                published_metadata = true;
            }
            publish_new_file(&temporary, &spec.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
            let _ = fs::remove_file(&metadata_temporary);
            if published_metadata { let _ = fs::remove_file(&metadata_target); }
        }
        result
    }
    pub fn export_snapshot(&self, id: &str) -> Result<ExportSnapshot> {
        Ok(self.exports.lock().unwrap().get(id).context("导出任务不存在")?.snapshot.lock().unwrap().clone())
    }
    pub fn cancel_export(&self, id: &str) -> Result<()> {
        self.exports.lock().unwrap().get(id).context("导出任务不存在")?.task.stop(); Ok(())
    }
    pub fn cache_usage(&self) -> serde_json::Value {
        let mut results = 0;
        let mut temporary = 0;
        let mut imports = 0;
        if let Ok(instances) = fs::read_dir(self.config.cache_dir.join("sessions")) {
            for instance in instances.filter_map(Result::ok) {
                let Ok(entries) = fs::read_dir(instance.path()) else { continue };
                for entry in entries.filter_map(Result::ok) {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with("query-") || name.starts_with("candidate-") { results += store::directory_size(&entry.path()); }
                    else if name == "spill" { temporary += store::directory_size(&entry.path()); }
                    else if name.starts_with("import-") { imports += store::directory_size(&entry.path()); }
                }
            }
        }
        serde_json::json!({
            "path": self.config.cache_dir,
            "datasets": store::directory_size(&self.config.cache_dir.join("datasets")).to_string(),
            "sessions": store::directory_size(&self.config.cache_dir.join("sessions")).to_string(),
            "limit": self.config.cache_limit_bytes.to_string(),
            "results": results.to_string(), "temporary": temporary.to_string(), "imports": imports.to_string()
        })
    }
    pub fn clear_unused_cache(&self) -> Result<u64> {
        self.evict_unused(true)
    }
    fn evict_unused(&self, all: bool) -> Result<u64> {
        let attachments = self.attachments.lock().unwrap();
        let pinned: Vec<_> = attachments.iter()
            .filter(|(_, a)| a.strong_count() > 0).map(|(p, _)| p.clone()).collect();
        let mut entries: Vec<_> = fs::read_dir(self.config.cache_dir.join("datasets"))?.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| fs::metadata(e.path().join("manifest.json")).and_then(|m| m.modified()).ok());
        let mut removed = 0;
        for entry in entries {
            let path = entry.path();
            if pinned.contains(&path) || !entry.file_type()?.is_dir() { continue; }
            if !all && store::directory_size(&self.config.cache_dir) < self.config.cache_limit_bytes.saturating_mul(9) / 10
                && fs2::available_space(&self.config.cache_dir)? > self.config.reserve_bytes + 256 * 1024 * 1024 { break; }
            let lease = self.dataset_lease(&path)?;
            if fs2::FileExt::try_lock_exclusive(&lease).is_err() { continue; }
            let size = store::directory_size(&path);
            fs::remove_dir_all(path)?;
            drop(lease);
            removed += size;
        }
        Ok(removed)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.candidates.get_mut().unwrap().clear();
        self.exports.get_mut().unwrap().clear();
        self.sessions.get_mut().unwrap().clear();
        self._instance_lock.take();
        let _ = fs::remove_dir_all(&self.instance_dir);
    }
}

fn filetime_touch(path: &Path) -> Result<()> {
    OpenOptions::new().write(true).open(path)?.set_modified(SystemTime::now())?;
    Ok(())
}

struct CsvBuffer(std::cell::RefCell<Vec<u8>>);
struct HashingWriter<W: Write> {
    output: W,
    hasher: blake3::Hasher,
}
impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.output.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> { self.output.flush() }
}
impl Write for CsvBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.get_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

fn flush_encoded(writer: &mut csv::Writer<CsvBuffer>, output: &mut impl Write, encoding: &str) -> Result<()> {
    writer.flush()?;
    {
        let bytes = writer.get_ref().0.borrow();
        if encoding == "utf-8" {
            output.write_all(&bytes)?;
        } else {
            let source = std::str::from_utf8(&bytes)?;
            let encoding = if encoding == "gbk" { encoding_rs::GBK } else { encoding_rs::GB18030 };
            let (encoded, _, errors) = encoding.encode(source);
            ensure!(!errors, "目标编码无法表示某些字符；请选择 UTF-8");
            output.write_all(&encoded)?;
        }
    }
    writer.get_ref().0.borrow_mut().clear();
    Ok(())
}

fn publish_new_file(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(windows)] {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        let from: Vec<u16> = fs::canonicalize(source)?.as_os_str().encode_wide().chain(Some(0)).collect();
        let parent = fs::canonicalize(destination.parent().context("无效导出路径")?)?;
        let target = parent.join(destination.file_name().context("缺少导出文件名")?);
        let to: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 {
            return Err(std::io::Error::last_os_error()).context("发布导出失败，目标可能已存在");
        }
    }
    #[cfg(not(windows))] {
        fs::hard_link(source, destination).context("无法发布到新文件")?;
        fs::remove_file(source)?;
    }
    Ok(())
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;

    #[test]
    fn watchdog_status_does_not_wait_for_busy_engine_locks() {
        let temp = tempfile::tempdir().unwrap();
        let engine = Engine::new(EngineConfig::new(temp.path())).unwrap();
        let _sessions = engine.sessions.lock().unwrap();
        let _waiting = engine.waiting.lock().unwrap();
        let _heavy = engine.heavy.lock().unwrap();
        let status = engine.diagnostic_status();
        assert_eq!(status["sessions"]["lockBusy"], "sessions");
        assert_eq!(status["waiting"]["lockBusy"], "waiting");
        assert_eq!(status["heavySlotBusy"], true);
    }
}
