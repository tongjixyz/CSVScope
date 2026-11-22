use anyhow::{Context, Result, ensure};
use csvscope_core::*;
use std::{collections::BTreeMap, fs::{self, File}, io::{BufWriter, Write}, path::PathBuf, sync::Arc, thread, time::{Duration, Instant}};

fn wait(engine: &Arc<Engine>, tab: &str) -> Result<SessionSnapshot> {
    loop {
        let state = engine.snapshot(tab)?;
        ensure!(!matches!(state.status.as_str(), "failed" | "cancelled" | "stale"), "{:?}", state.error);
        ensure!(state.query.as_ref().is_none_or(|q| !matches!(q.status.as_str(), "failed" | "cancelled")), "{:?}", state.query);
        if state.query.as_ref().is_some_and(|q| q.status == "ready") { return Ok(state); }
        thread::sleep(Duration::from_millis(5));
    }
}
fn equals(text: String) -> ColumnFilter {
    ColumnFilter { selection: None, predicates: vec![Predicate::Equals { text }] }
}
fn measure_candidates(engine: &Arc<Engine>, tab: &str, column: usize, search: &str) -> Result<serde_json::Value> {
    let started = Instant::now();
    let revision = engine.snapshot(tab)?.revision;
    let candidate = engine.candidates(tab, revision, column, search.into())?;
    loop {
        let state = engine.candidate_snapshot(&candidate)?;
        ensure!(state.status != "failed", "{:?}", state.error);
        if state.status == "ready" {
            let page = engine.candidate_page(&candidate, 0)?;
            let result = serde_json::json!({ "seconds": started.elapsed().as_secs_f64(), "distinctValues": state.count, "pageItems": page.items.len() });
            engine.close_candidate(&candidate);
            return Ok(result);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("multi") => {
            ensure!(args.len() >= 4, "multi <cache-directory> <file> [more files]");
            let engine = Engine::new(EngineConfig::new(&args[2]))?;
            let started = Instant::now();
            let tabs: Vec<_> = args[3..].iter().map(|p| engine.open(PathBuf::from(p), ParseSpec::default())).collect::<Result<_>>()?;
            let mut previews = BTreeMap::new();
            let mut ready = BTreeMap::new();
            loop {
                for tab in &tabs {
                    let state = engine.snapshot(tab)?;
                    ensure!(!matches!(state.status.as_str(), "failed" | "cancelled"), "{:?}", state.error);
                    ensure!(state.query.as_ref().is_none_or(|q| q.status != "failed"), "{:?}", state.query);
                    if !state.columns.is_empty() { previews.entry(tab.clone()).or_insert(started.elapsed().as_secs_f64()); }
                    if state.query.as_ref().is_some_and(|q| q.status == "ready") {
                        ready.entry(tab.clone()).or_insert(serde_json::json!({
                            "path": state.path, "bytes": state.total_bytes, "rows": state.rows_read,
                            "previewSeconds": previews.get(tab), "readySeconds": started.elapsed().as_secs_f64(), "cacheHit": state.cache_hit,
                        }));
                    }
                }
                if ready.len() == tabs.len() { break; }
                thread::sleep(Duration::from_millis(10));
            }
            println!("{}", serde_json::json!({ "status": "measured", "files": ready.values().collect::<Vec<_>>(),
                "totalSeconds": started.elapsed().as_secs_f64(), "cache": engine.cache_usage(), "note": "OS cache uncontrolled; one shared engine and one heavy-task permit." }));
        }
        Some("generate") => {
            let path = PathBuf::from(args.get(2).context("generate <file> <MiB> [columns] [unique]")?);
            ensure!(!path.exists(), "目标已经存在");
            let bytes: u64 = args.get(3).context("缺少 MiB")?.parse::<u64>()? * 1024 * 1024;
            let columns: usize = args.get(4).map(|v| v.parse()).transpose()?.unwrap_or(12);
            ensure!((5..=2048).contains(&columns), "列数范围 5–2048");
            let unique = args.get(5).is_some_and(|v| v == "unique");
            if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
            let mut file = BufWriter::with_capacity(1024 * 1024, File::create(&path)?);
            let headers = (0..columns).map(|i| format!("column_{i}")).collect::<Vec<_>>().join(",") + "\r\n";
            file.write_all(headers.as_bytes())?;
            let mut written = headers.len() as u64;
            let mut row = 0u64;
            let start = Instant::now();
            while written < bytes {
                let values = (0..columns).map(|c| match c {
                    0 => format!("小区{}", row % 100),
                    1 => ["1", "5", "9", "7"][(row % 4) as usize].to_string(),
                    2 => format!("00{:018}", if unique { row } else { row % 10000 }),
                    3 => format!("2026-11-18 12:{:02}:{:02}", (row / 60) % 60, row % 60),
                    4 => format!("\"test sample {}, \"\"quoted\"\"\"", row % 1000),
                    _ => if unique { format!("value_{c}_{row}") } else { format!("value_{c}_{}", row % 200) },
                }).collect::<Vec<_>>().join(",") + "\r\n";
                file.write_all(values.as_bytes())?;
                written += values.len() as u64;
                row += 1;
            }
            file.flush()?;
            println!("{}", serde_json::json!({ "path": path, "actualBytes": written, "rows": row, "columns": columns, "generationSeconds": start.elapsed().as_secs_f64() }));
        }
        Some("stream-filter") => {
            let path = PathBuf::from(args.get(2).context("stream-filter <generated-file> <new-cache-directory>")?);
            let cache = args.get(3).context("缺少新缓存目录")?;
            let engine = Engine::new(EngineConfig::new(cache))?;
            let opening = Instant::now();
            let tab = engine.open(path.clone(), ParseSpec::default())?;
            let before = loop {
                let state = engine.snapshot(&tab)?;
                ensure!(!matches!(state.status.as_str(), "failed" | "ready"), "未能在 EOF 前开始测量：{:?}", state.error);
                if state.query.as_ref().is_some_and(|q| q.status == "loading" && q.generated_rows.parse::<u64>().unwrap_or(0) > 200) {
                    break state;
                }
                thread::sleep(Duration::from_millis(5));
            };
            let text = engine.full_cell(&tab, 2, 0)?;
            let started = Instant::now();
            let filtered = engine.commit(&tab, before.revision, ViewState {
                filters: BTreeMap::from([(0, equals(text.clone()))]), ..Default::default()
            })?;
            let query_id = filtered.query.unwrap().id;
            let first = loop {
                let state = engine.snapshot(&tab)?;
                ensure!(state.query.as_ref().is_none_or(|q| q.status != "failed"), "{:?}", state.query);
                if state.query.as_ref().is_some_and(|q| q.generated_rows != "0") { break state; }
                thread::sleep(Duration::from_millis(1));
            };
            ensure!(first.status == "preparing", "首个匹配结果必须在 EOF 前可用");
            let rows = engine.copy_cells(&tab, &query_id, 0,
                first.query.as_ref().unwrap().generated_rows.parse::<usize>()?.min(256), vec![0], true)?;
            ensure!(!rows.is_empty() && rows.iter().all(|r| r[1] == text), "增量结果值不正确");
            ensure!(rows[0][0] == "2", "首条匹配记录的原始行号不正确");
            let first_page_seconds = started.elapsed().as_secs_f64();
            let complete = wait(&engine, &tab)?;
            let query = complete.query.as_ref().unwrap();
            ensure!(query.id == query_id, "EOF 后不应重新扫描已经筛选过的前缀");
            ensure!(query.generated_rows.parse::<u64>()? > first.query.as_ref().unwrap().generated_rows.parse::<u64>()?,
                "背景加载后应当补充新命中");
            println!("{}", serde_json::json!({
                "status": "measured", "path": path, "bytes": complete.total_bytes, "rows": complete.rows_read,
                "filterAppliedSecondsFromOpen": (opening.elapsed() - started.elapsed()).as_secs_f64(),
                "firstFilteredPageSeconds": first_page_seconds,
                "firstFilteredPage": { "availableSourceRows": first.rows_read,
                    "scannedSourceRows": first.query.as_ref().unwrap().scanned_rows,
                    "matches": first.query.as_ref().unwrap().generated_rows,
                    "returnedRows": rows.len(), "preparing": true, "exactTotal": first.query.as_ref().unwrap().exact_total },
                "readySeconds": opening.elapsed().as_secs_f64(), "finalMatches": query.exact_total,
                "sameQueryThroughEof": true, "cache": engine.cache_usage(),
                "note": "Fresh application cache; OS cache and other host workloads uncontrolled."
            }));
        }
        Some("open" | "exercise") => {
            let path = PathBuf::from(args.get(2).context("open <file> [cache-directory]")?);
            let cache = PathBuf::from(args.get(3).map(String::as_str).unwrap_or(".bench/cache"));
            let engine = Engine::new(EngineConfig::new(cache))?;
            let start = Instant::now();
            let tab = engine.open(path.clone(), ParseSpec::default())?;
            let mut preview_time = None;
            let mut ready_time = None;
            let mut loaded_page = None;
            loop {
                let state = engine.snapshot(&tab)?;
                if preview_time.is_none() && !state.columns.is_empty() {
                    preview_time = Some(start.elapsed().as_secs_f64());
                    eprintln!("preview: {:.3}s", preview_time.unwrap());
                }
                ensure!(!matches!(state.status.as_str(), "failed" | "cancelled"), "{:?}", state.error);
                if loaded_page.is_none() && state.status == "preparing" {
                    if let Some(query) = state.query.as_ref().filter(|q| q.status == "loading" && q.generated_rows.parse::<u64>().unwrap_or(0) > 200) {
                        let page_start = Instant::now();
                        let bytes = engine.page(&tab, &query.id, 200, 256, vec![0])?;
                        let batch = duckdb::arrow::ipc::reader::FileReader::try_new(std::io::Cursor::new(bytes), None)?
                            .next().context("后台读取的首个扩展页为空")??;
                        ensure!(batch.num_rows() > 0, "后台扩展页没有记录");
                        loaded_page = Some(serde_json::json!({
                            "secondsFromOpen": start.elapsed().as_secs_f64(),
                            "readSeconds": page_start.elapsed().as_secs_f64(),
                            "availableRows": query.generated_rows, "pageRows": batch.num_rows(),
                            "start": 200, "preparing": true,
                        }));
                    }
                }
                if state.status == "ready" && ready_time.is_none() {
                    ready_time = Some(start.elapsed().as_secs_f64());
                }
                if state.query.as_ref().is_some_and(|q| q.status == "ready") {
                    let mut report = serde_json::json!({
                        "status": "measured", "path": path, "bytes": state.total_bytes, "rows": state.rows_read,
                        "columns": state.columns.len(), "previewSeconds": preview_time,
                        "readySeconds": ready_time, "firstLoadedPage": loaded_page, "cacheHit": state.cache_hit, "cache": engine.cache_usage(),
                        "note": "OS file-cache state is not controlled by this command; process-tree memory requires the benchmark wrapper."
                    });
                    if args[1] == "exercise" {
                        ensure!(state.columns.len() >= 5 && state.rows_read.parse::<u64>()? > 1, "exercise 用于 csvbench 生成的至少 5 列数据");
                        let selected = engine.full_cell(&tab, 2, 0)?;
                        let mut view = ViewState { filters: BTreeMap::from([(0, equals(selected))]), sort: vec![], ..Default::default() };
                        let query_start = Instant::now();
                        engine.commit(&tab, state.revision, view.clone())?;
                        let first = wait(&engine, &tab)?;
                        report["firstFilterSeconds"] = query_start.elapsed().as_secs_f64().into();
                        report["firstFilterRows"] = first.query.as_ref().unwrap().exact_total.clone().into();
                        let mut changes = vec![];
                        for rid in 3..6 {
                            view.filters.insert(0, equals(engine.full_cell(&tab, rid, 0)?));
                            let started = Instant::now();
                            engine.commit(&tab, engine.snapshot(&tab)?.revision, view.clone())?;
                            wait(&engine, &tab)?;
                            changes.push(started.elapsed().as_secs_f64());
                        }
                        report["consecutiveFilterSeconds"] = serde_json::to_value(changes)?;
                        report["linkedCandidates"] = measure_candidates(&engine, &tab, 1, "")?;
                        report["candidateSearch"] = measure_candidates(&engine, &tab, 2, "001")?;
                        let filtered = wait(&engine, &tab)?;
                        let export_path = engine.config.cache_dir.join(format!("benchmark-export-{}.csv", uuid::Uuid::new_v4()));
                        let export_start = Instant::now();
                        let export_id = engine.export(&tab, &filtered.query.as_ref().unwrap().id, ExportSpec {
                            path: export_path.clone(), columns: (0..state.columns.len()).collect(), delimiter: ",".into(),
                            encoding: "utf-8".into(), bom: false, headers: true, all_rows: false,
                        })?;
                        loop {
                            let export = engine.export_snapshot(&export_id)?;
                            ensure!(export.status != "failed", "{:?}", export.error);
                            if export.status == "completed" {
                                let count = csv::Reader::from_path(&export_path)?.records().try_fold(0u64, |n, row| row.map(|_| n + 1))?;
                                ensure!(count.to_string() == export.rows_written, "导出校验数量不一致");
                                report["export"] = serde_json::json!({
                                    "secondsIncludingVerification": export_start.elapsed().as_secs_f64(),
                                    "rows": count, "bytes": fs::metadata(&export_path)?.len(), "verified": true,
                                });
                                fs::remove_file(export_path)?;
                                break;
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                        let sort_start = Instant::now();
                        engine.commit(&tab, filtered.revision, ViewState {
                            filters: BTreeMap::new(), sort: vec![SortKey { column: 2, descending: true, interpretation: "text".into() }],
                            ..Default::default()
                        })?;
                        let sorted = wait(&engine, &tab)?;
                        report["fullSortSeconds"] = sort_start.elapsed().as_secs_f64().into();
                        report["cacheAfterFullSort"] = engine.cache_usage();
                        let query = sorted.query.as_ref().unwrap();
                        let total = query.exact_total.as_ref().unwrap().parse::<u64>()?;
                        let started = Instant::now();
                        for position in [total / 2, total * 9 / 10, total.saturating_sub(1)] {
                            engine.page(&tab, &query.id, position, 128, vec![0, 1, 2])?;
                        }
                        report["threeFarPagesSeconds"] = started.elapsed().as_secs_f64().into();
                        let scroll = Instant::now();
                        for page in 0..20 {
                            if page * 256 < total { engine.page(&tab, &query.id, page * 256, 256, vec![0, 1, 2])?; }
                        }
                        report["twentySequentialPagesSeconds"] = scroll.elapsed().as_secs_f64().into();
                        engine.commit(&tab, sorted.revision, ViewState::default())?;
                        wait(&engine, &tab)?;
                        report["fullColumnCandidates"] = measure_candidates(&engine, &tab, 2, "")?;
                        let before_cancel = engine.snapshot(&tab)?;
                        engine.commit(&tab, before_cancel.revision, ViewState {
                            filters: BTreeMap::new(), sort: vec![SortKey { column: 4, descending: true, interpretation: "text".into() }],
                            ..Default::default()
                        })?;
                        thread::sleep(Duration::from_millis(20));
                        let started = Instant::now();
                        engine.cancel(&tab)?;
                        loop {
                            let state = engine.snapshot(&tab)?;
                            if state.query.as_ref().is_some_and(|q| ["cancelled", "ready"].contains(&q.status.as_str())) {
                                report["cancelSeconds"] = started.elapsed().as_secs_f64().into();
                                report["cancelFinalStatus"] = state.query.unwrap().status.into();
                                break;
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                        report["cacheAfterExercise"] = engine.cache_usage();
                    }
                    println!("{report}");
                    break;
                }
                ensure!(state.query.as_ref().is_none_or(|q| q.status != "failed"), "{:?}", state.query);
                thread::sleep(Duration::from_millis(10));
            }
        }
        _ => println!("csvbench generate <new-file> <MiB> [columns] [unique]\ncsvbench open <file> [cache-directory]\ncsvbench stream-filter <generated-file> <new-cache-directory>\ncsvbench exercise <generated-file> [cache-directory]\ncsvbench multi <cache-directory> <files...>"),
    }
    Ok(())
}
