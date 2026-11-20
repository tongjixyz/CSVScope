use csvscope_core::*;
use std::{collections::BTreeMap, fs, path::Path, sync::Arc, thread, time::{Duration, Instant}};

fn setup(path: &Path) -> Arc<Engine> {
    let mut config = EngineConfig::new(path);
    config.reserve_bytes = 0;
    config.memory_limit_mb = 256;
    Engine::new(config).unwrap()
}

fn ready(engine: &Arc<Engine>, tab: &str) -> SessionSnapshot {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = engine.snapshot(tab).unwrap();
        assert_ne!(state.status, "failed", "{:?}", state.error);
        assert_ne!(state.query.as_ref().map(|q| q.status.as_str()), Some("failed"), "{:?}", state.query);
        if state.query.as_ref().is_some_and(|q| q.status == "ready" && q.revision == state.revision) { return state; }
        assert!(Instant::now() < deadline, "{state:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn commit(engine: &Arc<Engine>, tab: &str, view: ViewState) -> SessionSnapshot {
    engine.commit(tab, engine.snapshot(tab).unwrap().revision, view).unwrap();
    ready(engine, tab)
}

fn exported(engine: &Arc<Engine>, tab: &str, path: &Path, columns: Vec<usize>, all_rows: bool) {
    let state = ready(engine, tab);
    let id = engine.export(tab, &state.query.unwrap().id, ExportSpec {
        path: path.into(), columns, delimiter: ",".into(), encoding: "utf-8".into(),
        bom: false, headers: true, all_rows,
    }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = engine.export_snapshot(&id).unwrap();
        assert_ne!(status.status, "failed", "{:?}", status.error);
        if status.status == "completed" { return; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn text_edits_use_full_values_in_pages_filters_candidates_and_history_without_changing_other_tabs() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("text.csv");
    let original = "A,B,C\nx,001,one\ny,002,two\nz,003,three\n";
    fs::write(&source, original).unwrap();
    let engine = setup(&workspace.path().join("cache"));
    let tab = engine.open(source.clone(), ParseSpec::default()).unwrap();
    let second = engine.open(source.clone(), ParseSpec::default()).unwrap();
    ready(&engine, &second);
    let mut view = ready(&engine, &tab).view;
    let text = format!("00'1,\r\n\"改\"{}尾部", "长".repeat(1200));
    view.edits.cells.insert(1, BTreeMap::from([(1, text.clone()), (2, "a\0b".into())]));
    view.edits.cells.insert(2, BTreeMap::from([(0, "x".into())]));
    let edited = commit(&engine, &tab, view.clone());
    assert!(edited.modified);
    assert_eq!(engine.full_cell(&tab, 1, 1).unwrap(), text);
    assert_eq!(engine.full_cell(&tab, 1, 2).unwrap(), "a\0b");
    let cells = engine.copy_cells(&tab, &edited.query.unwrap().id, 0, 2, vec![0, 1, 2], true).unwrap();
    assert_eq!(cells[0], ["1", "x", &text, "a\0b"]);
    assert_eq!(cells[1], ["2", "x", "002", "two"]);
    assert_eq!(engine.full_cell(&second, 1, 1).unwrap(), "001");
    assert_eq!(engine.full_cell(&second, 2, 0).unwrap(), "y");
    view.filters.insert(0, ColumnFilter { selection: None, predicates: vec![Predicate::Equals { text: "x".into() }] });
    view.sort = vec![SortKey { column: 1, descending: true, interpretation: "text".into() }];
    let filtered = commit(&engine, &tab, view);
    assert_eq!(filtered.query.as_ref().unwrap().exact_total.as_deref(), Some("2"));
    let id = engine.candidates(&tab, filtered.revision, 1, "尾部".into()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.candidate_snapshot(&id).unwrap().status != "ready" {
        assert_ne!(engine.candidate_snapshot(&id).unwrap().status, "failed");
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(engine.candidate_page(&id, 0).unwrap().exact_total.as_deref(), Some("1"));
    engine.history(&tab, filtered.revision, false).unwrap();
    let restored = ready(&engine, &tab);
    assert!(restored.view.filters.is_empty());
    assert_eq!(engine.full_cell(&tab, 1, 1).unwrap(), text);
    engine.history(&tab, restored.revision, false).unwrap();
    assert!(!ready(&engine, &tab).modified);
    assert_eq!(engine.full_cell(&tab, 1, 1).unwrap(), "001");
    assert_eq!(fs::read_to_string(source).unwrap(), original);
}

#[test]
fn deleted_rows_columns_and_color_identity_round_trip_through_sorted_all_and_filtered_csv() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("source.csv");
    fs::write(&source, "A,B,C\nx,001,one\ny,002,two\nz,003,three\n").unwrap();
    let engine = setup(&workspace.path().join("cache"));
    let tab = engine.open(source, ParseSpec::default()).unwrap();
    let mut view = ready(&engine, &tab).view;
    view.edits.cells.insert(2, BTreeMap::from([(0, "x".into())]));
    view.edits.deleted_rows.insert(1);
    view.edits.deleted_columns.insert(2);
    view.edits.colors.insert(2, BTreeMap::from([(1, "#fff1b8".into())]));
    view.edits.colors.insert(3, BTreeMap::from([(0, "#d9eafa".into()), (2, "#f9dce6".into())]));
    view.filters.insert(0, ColumnFilter { selection: None, predicates: vec![Predicate::Equals { text: "x".into() }] });
    view.sort = vec![SortKey { column: 0, descending: true, interpretation: "text".into() }];
    let state = commit(&engine, &tab, view);
    assert_eq!(state.query.as_ref().unwrap().exact_total.as_deref(), Some("1"));
    let all = workspace.path().join("all.csv");
    exported(&engine, &tab, &all, vec![1, 0], true);
    let rows = csv::Reader::from_path(&all).unwrap().records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].iter().collect::<Vec<_>>(), ["003", "z"]);
    assert_eq!(rows[1].iter().collect::<Vec<_>>(), ["002", "x"]);
    let reopened = engine.open(all.clone(), ParseSpec::default()).unwrap();
    let saved = ready(&engine, &reopened);
    assert!(!saved.modified, "loaded colors form the saved baseline");
    assert_eq!(saved.view.edits.colors[&1], BTreeMap::from([(1, "#d9eafa".into())]));
    assert_eq!(saved.view.edits.colors[&2], BTreeMap::from([(0, "#fff1b8".into())]));
    let filtered = workspace.path().join("filtered.csv");
    exported(&engine, &tab, &filtered, vec![0, 1], false);
    let filtered_tab = engine.open(filtered, ParseSpec::default()).unwrap();
    let filtered_state = ready(&engine, &filtered_tab);
    assert_eq!(filtered_state.rows_read, "1");
    assert_eq!(filtered_state.view.edits.colors[&1][&1], "#fff1b8");
    engine.history(&tab, state.revision, false).unwrap();
    assert_eq!(ready(&engine, &tab).query.unwrap().exact_total.as_deref(), Some("3"));
    fs::write(&all, "B,A\n003,externally-changed\n002,x\n").unwrap();
    let changed = engine.open(all, ParseSpec::default()).unwrap();
    let changed = ready(&engine, &changed);
    assert!(changed.view.edits.colors.is_empty());
    assert!(changed.warnings.iter().any(|warning| warning.contains("未加载颜色标记")));
}

#[test]
fn invalid_edits_and_deleted_column_exports_are_rejected_without_partial_state() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("small.csv");
    fs::write(&source, "A,B\nx,001\n").unwrap();
    let engine = setup(&workspace.path().join("cache"));
    let tab = engine.open(source, ParseSpec::default()).unwrap();
    let initial = ready(&engine, &tab);
    for edits in [
        EditState { deleted_rows: [0].into(), ..Default::default() },
        EditState { deleted_rows: [2].into(), ..Default::default() },
        EditState { deleted_columns: [0, 1].into(), ..Default::default() },
        EditState { cells: BTreeMap::from([(1, BTreeMap::from([(9, "oops".into())]))]), ..Default::default() },
        EditState { colors: BTreeMap::from([(1, BTreeMap::from([(0, "red;evil".into())]))]), ..Default::default() },
    ] {
        assert!(engine.commit(&tab, initial.revision, ViewState { edits, ..Default::default() }).is_err());
        assert_eq!(engine.snapshot(&tab).unwrap().revision, initial.revision);
    }
    let mut view = initial.view;
    view.edits.deleted_columns.insert(1);
    let state = commit(&engine, &tab, view);
    assert!(engine.export(&tab, &state.query.unwrap().id, ExportSpec {
        path: workspace.path().join("bad.csv"), columns: vec![0, 1], delimiter: ",".into(),
        encoding: "utf-8".into(), bom: false, headers: true, all_rows: true,
    }).is_err());
    assert!(!workspace.path().join("bad.csv").exists());
}

#[test]
fn deleting_all_rows_saves_headers_and_can_be_undone() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("empty.csv");
    fs::write(&source, "A,B\nx,001\n").unwrap();
    let engine = setup(&workspace.path().join("cache"));
    let tab = engine.open(source, ParseSpec::default()).unwrap();
    let mut view = ready(&engine, &tab).view;
    view.edits.deleted_rows.insert(1);
    let state = commit(&engine, &tab, view);
    assert_eq!(state.query.unwrap().exact_total.as_deref(), Some("0"));
    let output = workspace.path().join("result.csv");
    exported(&engine, &tab, &output, vec![0, 1], true);
    assert_eq!(fs::read_to_string(output).unwrap(), "A,B\r\n");
    engine.history(&tab, state.revision, false).unwrap();
    assert_eq!(ready(&engine, &tab).query.unwrap().exact_total.as_deref(), Some("1"));
}
