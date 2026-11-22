use csvscope_core::{Engine, diagnostics as log};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, Ordering}},
    thread,
    time::{Duration, Instant},
};

pub fn init_logging() {
    let mut directories = Vec::new();
    if let Some(path) = std::env::var_os("CSVSCOPE_LOG_DIR") { directories.push(PathBuf::from(path)); }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() { directories.push(parent.join("logs")); }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        directories.push(PathBuf::from(local).join("io.github.tongjixyz.csvscope").join("logs"));
    }
    directories.push(std::env::temp_dir().join("CSVScope").join("logs"));
    let mut failures = Vec::new();
    for directory in directories {
        match log::init(&directory) {
            Ok(file) => {
                log::record("info", "app_start", json!({
                    "version": env!("CARGO_PKG_VERSION"), "diagnosticsVersion": 1,
                    "executable": std::env::current_exe().ok(), "logFile": file,
                    "workingDirectory": std::env::current_dir().ok(),
                    "availableParallelism": thread::available_parallelism().map(|n| n.get()).ok(),
                    "logDirectoryFallbacks": failures,
                }));
                break;
            }
            Err(error) => failures.push(json!({ "directory": directory, "error": error.to_string() })),
        }
    }
    log::install_panic_hook();
}

pub struct Health {
    started: Instant,
    running: AtomicBool,
    native_pending: AtomicBool,
    native_ack: AtomicU64,
    frontend_ack: AtomicU64,
    frontend_visible: AtomicBool,
    frontend_pending: Mutex<Value>,
}

impl Health {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(), running: AtomicBool::new(true), native_pending: AtomicBool::new(false),
            native_ack: AtomicU64::new(0), frontend_ack: AtomicU64::new(0),
            frontend_visible: AtomicBool::new(true), frontend_pending: Mutex::new(json!([])),
        })
    }
    fn elapsed(&self) -> u64 { self.started.elapsed().as_millis() as u64 }
    pub fn frontend_heartbeat(&self, visible: bool, pending: Value) {
        self.frontend_ack.store(self.elapsed(), Ordering::Relaxed);
        self.frontend_visible.store(visible, Ordering::Relaxed);
        if let Ok(mut previous) = self.frontend_pending.try_lock() { *previous = pending; }
    }
    pub fn stop(&self) { self.running.store(false, Ordering::Relaxed); }
}

pub fn start_watchdog(app: tauri::AppHandle, health: Arc<Health>, engine: &Arc<Engine>) {
    let engine = Arc::downgrade(engine);
    thread::Builder::new().name("ui-watchdog".into()).spawn(move || {
        let mut iteration = 0;
        let mut native_stalled = false;
        let mut frontend_stalled = false;
        while health.running.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_secs(2));
            if !health.running.load(Ordering::Relaxed) { break; }
            let now = health.elapsed();
            let native_gap = now.saturating_sub(health.native_ack.load(Ordering::Relaxed));
            let frontend_gap = now.saturating_sub(health.frontend_ack.load(Ordering::Relaxed));
            let native_late = native_gap >= 5000;
            let frontend_late = frontend_gap >= 10000 && health.frontend_visible.load(Ordering::Relaxed);
            let changed = native_stalled != native_late || frontend_stalled != frontend_late;
            if changed || iteration % 5 == 0 {
                let operations = log::active_operations();
                let slow = operations.as_array().is_some_and(|ops|
                    ops.iter().any(|op| op["elapsedMs"].as_u64().unwrap_or(0) >= 5000));
                if changed || native_late || frontend_late || slow || iteration % 15 == 0 {
                    log::record(if native_late || frontend_late || slow { "warn" } else { "info" },
                        if native_late || frontend_late { "ui_unresponsive" }
                        else if native_stalled || frontend_stalled { "ui_recovered" }
                        else { "runtime_status" }, json!({
                            "uptimeMs": now, "nativeHeartbeatGapMs": native_gap,
                            "frontendHeartbeatGapMs": frontend_gap,
                            "frontendVisible": health.frontend_visible.load(Ordering::Relaxed),
                            "pendingFrontendRequests": health.frontend_pending.try_lock().ok().map(|value| value.clone()),
                            "activeOperations": operations,
                            "engine": engine.upgrade().map(|engine| engine.diagnostic_status()),
                        }));
                }
            }
            native_stalled = native_late;
            frontend_stalled = frontend_late;
            if !health.native_pending.swap(true, Ordering::Relaxed) {
                let probe = health.clone();
                if let Err(error) = app.run_on_main_thread(move || {
                    probe.native_ack.store(probe.elapsed(), Ordering::Relaxed);
                    probe.native_pending.store(false, Ordering::Relaxed);
                }) {
                    log::record("error", "native_probe_failed", json!({ "error": error.to_string() }));
                    break;
                }
            }
            iteration += 1;
        }
    }).expect("无法创建卡顿监测线程");
}
