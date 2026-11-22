use super::*;
use std::time::Instant;

const ROWS: u64 = 131_072;

fn wait_for(engine: &Engine, tab: &str, predicate: impl Fn(&SessionSnapshot) -> bool) -> SessionSnapshot {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let snapshot = engine.snapshot(tab).unwrap();
        if predicate(&snapshot) { return snapshot; }
        assert_ne!(snapshot.status, "failed", "{:?}", snapshot.error);
        assert!(Instant::now() < deadline, "{snapshot:?}");
        thread::sleep(Duration::from_millis(1));
    }
}

fn fixture(bad_tail: bool) -> (tempfile::TempDir, Arc<Engine>, String, String) {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("stream.csv");
    let long = "完整内容".repeat(400) + ",\r\n\"结尾\"";
    let payload = "带逗号,双\"引号\r\n".repeat(8);
    let mut writer = csv::Writer::from_path(&source).unwrap();
    writer.write_record(["编号", "分组", "说明"]).unwrap();
    for row in 1..=ROWS {
        writer.write_record([format!("{row:08}"), (row % 2).to_string(),
            if row == 1 { long.clone() } else { payload.clone() }]).unwrap();
    }
    writer.flush().unwrap();
    drop(writer);
    if bad_tail { OpenOptions::new().append(true).open(&source).unwrap().write_all(b"bad,width\n").unwrap(); }
    let mut config = EngineConfig::new(workspace.path().join("cache"));
    config.reserve_bytes = 0;
    config.memory_limit_mb = 256;
    let engine = Engine::new(config).unwrap();
    let tab = engine.open(source, ParseSpec::default()).unwrap();
    (workspace, engine, tab, long)
}

#[test]
fn loaded_rows_can_be_filtered_copied_and_undone_before_eof() {
    let (_workspace, engine, tab, long) = fixture(false);
    wait_for(&engine, &tab, |s| s.query.as_ref().is_some_and(|q|
        q.status == "loading" && q.generated_rows.parse::<u64>().unwrap() > 200));
    let pause_import = engine.heavy.lock().unwrap();
    let snapshot = engine.snapshot(&tab).unwrap();
    assert_eq!(snapshot.status, "preparing");
    let query = snapshot.query.unwrap();
    assert_eq!(query.exact_total, None, "a growing prefix must not claim a complete count");
    let loaded: u64 = query.generated_rows.parse().unwrap();
    assert!(loaded < ROWS);
    let bytes = engine.page(&tab, &query.id, 300, 256, vec![0, 2]).unwrap();
    let batch = duckdb::arrow::ipc::reader::FileReader::try_new(std::io::Cursor::new(bytes), None)
        .unwrap().next().unwrap().unwrap();
    assert_eq!(batch.num_rows(), 256);
    assert_eq!(array_value_to_string(batch.column(0).as_ref(), 0).unwrap(), "301");
    assert_eq!(engine.full_cell(&tab, 1, 2).unwrap(), long);
    assert_eq!(engine.copy_cells(&tab, &query.id, 0, 1, vec![0, 2], true).unwrap()[0],
        ["1".to_string(), "00000001".to_string(), long]);
    assert!(engine.page(&tab, &query.id, ROWS, 1, vec![0]).is_err());
    assert!(engine.copy_cells(&tab, &query.id, ROWS - 1, 2, vec![0], false).is_err());
    assert!(engine.page(&tab, "old-query", 0, 1, vec![0]).is_err());

    let mut view = ViewState::default();
    view.filters.insert(1, ColumnFilter { selection: None, predicates: vec![Predicate::Equals { text: "0".into() }] });
    let queued = engine.commit(&tab, snapshot.revision, view.clone()).unwrap();
    assert_eq!(queued.query.as_ref().unwrap().revision, queued.revision);
    assert_eq!(queued.query.as_ref().unwrap().status, "loading");
    let filtered = wait_for(&engine, &tab, |s| s.query.as_ref().is_some_and(|q|
        q.scanned_rows.as_deref() == Some(s.rows_read.as_str()) && q.generated_rows.parse::<u64>().unwrap() > 0));
    assert_eq!(filtered.status, "preparing");
    let partial = filtered.query.unwrap();
    assert_eq!(partial.exact_total, None);
    assert_eq!(partial.generated_rows.parse::<u64>().unwrap(), filtered.rows_read.parse::<u64>().unwrap() / 2);
    assert_eq!(engine.copy_cells(&tab, &partial.id, 0, 2, vec![0, 1], true).unwrap(),
        [vec!["2", "00000002", "0"], vec!["4", "00000004", "0"]]);
    assert!(engine.page(&tab, &query.id, 0, 1, vec![0]).is_err(), "obsolete results must be retired");
    view.edits.cells.insert(1, BTreeMap::from([(0, "edited".into())]));
    assert!(engine.commit(&tab, queued.revision, view).is_err());
    let undone = engine.history(&tab, queued.revision, false).unwrap();
    assert_eq!(undone.query.as_ref().unwrap().revision, undone.revision);
    assert_eq!(engine.copy_cells(&tab, &undone.query.unwrap().id, 0, 1, vec![1], true).unwrap()[0], ["1", "1"]);
    let redone = engine.history(&tab, undone.revision, true).unwrap();
    wait_for(&engine, &tab, |s| s.query.as_ref().is_some_and(|q| q.generated_rows != "0"));
    let incremental_id = redone.query.unwrap().id;
    drop(pause_import);

    let ready = wait_for(&engine, &tab, |s| s.status == "ready" && s.query.as_ref().is_some_and(|q| q.status == "ready"));
    assert_eq!(ready.rows_read, ROWS.to_string());
    let completed = ready.query.unwrap();
    assert_eq!(completed.id, incremental_id, "EOF must retain the incremental result rather than rescan the prefix");
    assert_eq!(completed.exact_total, Some((ROWS / 2).to_string()));
    assert_eq!(engine.copy_cells(&tab, &completed.id, 0, 1, vec![0], true).unwrap()[0], ["2", "00000002"]);
    assert!(engine.page(&tab, &query.id, 0, 1, vec![0]).is_err());
}

#[test]
fn late_matches_multicolumn_rules_and_partial_candidates_work_before_eof() {
    let (_workspace, engine, tab, _) = fixture(false);
    wait_for(&engine, &tab, |s| s.query.as_ref().is_some_and(|q| q.status == "loading"));
    let pause_import = engine.heavy.lock().unwrap();
    let before = engine.snapshot(&tab).unwrap();
    let loaded = before.rows_read.parse::<u64>().unwrap();
    assert!(loaded < ROWS);
    let boundary = loaded + 10;
    let mut view = ViewState::default();
    view.filters.insert(0, ColumnFilter { selection: None, predicates: vec![
        Predicate::NumericRange { min: Some(boundary.to_string()), max: None },
    ] });
    view.filters.insert(1, ColumnFilter { selection: Some(ValueSelection {
        default_selected: false, rules: vec![SearchRule { search: "0".into(), selected: true }], overrides: vec![],
    }), predicates: vec![] });
    let filtered = engine.commit(&tab, before.revision, view).unwrap();
    let empty = wait_for(&engine, &tab, |s| s.query.as_ref().is_some_and(|q| q.scanned_rows.as_deref() == Some(s.rows_read.as_str())));
    assert_eq!(empty.status, "preparing");
    assert_eq!(empty.query.as_ref().unwrap().generated_rows, "0");
    assert_eq!(empty.query.as_ref().unwrap().exact_total, None, "an empty prefix is not a definitive empty result");

    // Candidates exclude the current column's filter. They must be usable while
    // the importer is paused, and must not claim a full-file distinct count.
    let candidate = engine.candidates(&tab, filtered.revision, 0, "00000".into()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = engine.candidate_snapshot(&candidate).unwrap();
        assert_ne!(state.status, "failed", "{:?}", state.error);
        if state.status == "ready" {
            assert!(state.partial);
            let page = engine.candidate_page(&candidate, 0).unwrap();
            assert_eq!(page.exact_total, None);
            assert!(!page.items.is_empty());
            assert!(page.items.iter().all(|v| v.label.parse::<u64>().unwrap() % 2 == 0));
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    engine.close_candidate(&candidate);
    drop(pause_import);
    let ready = wait_for(&engine, &tab, |s| s.query.as_ref().is_some_and(|q| q.status == "ready"));
    let query = ready.query.unwrap();
    assert_eq!(query.id, filtered.query.unwrap().id);
    let first = if boundary % 2 == 0 { boundary } else { boundary + 1 };
    assert_eq!(query.exact_total, Some(((ROWS - first) / 2 + 1).to_string()));
    assert_eq!(engine.copy_cells(&tab, &query.id, 0, 1, vec![1], true).unwrap()[0], [first.to_string(), "0".into()]);
}

#[test]
fn sorting_is_deferred_but_filters_are_live_and_final_order_is_global() {
    let (_workspace, engine, tab, _) = fixture(false);
    wait_for(&engine, &tab, |s| s.query.as_ref().is_some_and(|q| q.status == "loading"));
    let pause_import = engine.heavy.lock().unwrap();
    let before = engine.snapshot(&tab).unwrap();
    let view = ViewState {
        filters: BTreeMap::from([(1, ColumnFilter { selection: None, predicates: vec![Predicate::Equals { text: "0".into() }] })]),
        sort: vec![SortKey { column: 0, descending: true, interpretation: "number".into() }],
        ..Default::default()
    };
    let partial = engine.commit(&tab, before.revision, view).unwrap();
    let filtered = wait_for(&engine, &tab, |s| s.query.as_ref().is_some_and(|q| q.generated_rows != "0"));
    assert_eq!(filtered.status, "preparing");
    assert_eq!(engine.copy_cells(&tab, &filtered.query.unwrap().id, 0, 1, vec![], true).unwrap()[0], ["2"]);
    drop(pause_import);
    let ready = wait_for(&engine, &tab, |s| s.query.as_ref().is_some_and(|q| q.status == "ready"));
    let query = ready.query.unwrap();
    assert_ne!(query.id, partial.query.unwrap().id, "global sorting replaces the provisional source order");
    assert_eq!(query.exact_total, Some((ROWS / 2).to_string()));
    assert_eq!(engine.copy_cells(&tab, &query.id, 0, 2, vec![], true).unwrap(),
        [vec![ROWS.to_string()], vec![(ROWS - 2).to_string()]]);
}

#[test]
fn cancelling_background_loading_retires_partial_pages_without_publishing_cache() {
    let (_workspace, engine, tab, _) = fixture(false);
    wait_for(&engine, &tab, |s| s.query.as_ref().is_some_and(|q| q.status == "loading"));
    let pause_import = engine.heavy.lock().unwrap();
    let snapshot = engine.snapshot(&tab).unwrap();
    let filtered = engine.commit(&tab, snapshot.revision, ViewState {
        filters: BTreeMap::from([(1, ColumnFilter { selection: None, predicates: vec![Predicate::Equals { text: "0".into() }] })]),
        ..Default::default()
    }).unwrap();
    wait_for(&engine, &tab, |s| s.query.as_ref().is_some_and(|q| q.generated_rows != "0"));
    let query_id = filtered.query.unwrap().id;
    engine.cancel(&tab).unwrap();
    drop(pause_import);
    let cancelled = wait_for(&engine, &tab, |s| s.status == "cancelled");
    assert!(cancelled.query.is_none());
    assert!(engine.page(&tab, &query_id, 0, 1, vec![0]).is_err());
    assert_eq!(fs::read_dir(engine.config.cache_dir.join("datasets")).unwrap().count(), 0);
}

#[test]
fn malformed_tail_invalidates_partial_pages_instead_of_reporting_ready() {
    let (_workspace, engine, tab, _) = fixture(true);
    wait_for(&engine, &tab, |s| s.query.as_ref().is_some_and(|q| q.status == "loading"));
    let pause_import = engine.heavy.lock().unwrap();
    let snapshot = engine.snapshot(&tab).unwrap();
    engine.commit(&tab, snapshot.revision, ViewState {
        filters: BTreeMap::from([(1, ColumnFilter { selection: None, predicates: vec![Predicate::Equals { text: "0".into() }] })]),
        ..Default::default()
    }).unwrap();
    wait_for(&engine, &tab, |s| s.query.as_ref().is_some_and(|q| q.generated_rows != "0"));
    drop(pause_import);
    let failed = wait_for(&engine, &tab, |s| s.status == "failed");
    assert!(failed.query.is_none());
    assert!(failed.error.unwrap().contains("实际 2 列"));
    assert_eq!(fs::read_dir(engine.config.cache_dir.join("datasets")).unwrap().count(), 0);
}
