use csvscope_core::*;
use duckdb::arrow::ipc::reader::FileReader;
use std::{collections::BTreeMap, fs::{self, File}, io::Cursor, path::Path, sync::Arc, thread, time::{Duration, Instant}};

fn wait(engine: &Arc<Engine>, tab: &str) -> SessionSnapshot {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let snapshot = engine.snapshot(tab).unwrap();
        assert!(!matches!(snapshot.status.as_str(), "failed" | "cancelled"), "{:?}", snapshot.error);
        if let Some(query) = snapshot.query.as_ref().filter(|query| snapshot.status == "ready" && query.status == "ready") {
            assert_eq!(query.exact_total.as_deref(), Some(query.generated_rows.as_str()));
            return snapshot;
        }
        assert!(snapshot.query.as_ref().is_none_or(|q| q.status != "failed"), "{:?}", snapshot.query);
        assert!(Instant::now() < deadline, "timeout: {:?}", snapshot);
        thread::sleep(Duration::from_millis(10));
    }
}
fn candidates(engine: &Arc<Engine>, tab: &str, column: usize, search: &str) -> Vec<String> {
    let revision = engine.snapshot(tab).unwrap().revision;
    let id = engine.candidates(tab, revision, column, search.into()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let snapshot = engine.candidate_snapshot(&id).unwrap();
        assert_ne!(snapshot.status, "failed", "{:?}", snapshot.error);
        if snapshot.status == "ready" {
            return engine.candidate_page(&id, 0).unwrap().items.into_iter().map(|i| i.label).collect();
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
}
fn selected(values: &[&str]) -> ColumnFilter {
    ColumnFilter {
        selection: Some(ValueSelection {
            default_selected: false, rules: vec![],
            overrides: values.iter().map(|v| ValueOverride { value: FilterValue::text(*v), selected: true, after_rule: 0 }).collect(),
        }),
        predicates: vec![],
    }
}
fn setup(path: &Path) -> Arc<Engine> {
    let mut config = EngineConfig::new(path);
    config.reserve_bytes = 0;
    config.memory_limit_mb = 256;
    Engine::new(config).unwrap()
}

#[test]
fn linked_filters_history_export_and_cache_are_consistent() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("linked.csv");
    fs::write(&source, "小区,QCI,编号,说明\r\n小区1,1,001,\"a,\r\nb\"\r\n小区1,5,002,\"双\"\"引号\"\r\n小区2,5,003,\r\n小区2,9,004, \r\n").unwrap();
    let engine = setup(&workspace.path().join("cache"));
    let tab = engine.open(source.clone(), ParseSpec::default()).unwrap();
    let initial = wait(&engine, &tab);
    assert_eq!(initial.query.unwrap().exact_total.as_deref(), Some("4"));
    let mut view = ViewState::default();
    view.filters.insert(0, selected(&["小区1"]));
    engine.commit(&tab, initial.revision, view.clone()).unwrap();
    assert_eq!(wait(&engine, &tab).query.unwrap().exact_total.as_deref(), Some("2"));
    assert_eq!(candidates(&engine, &tab, 1, ""), ["1", "5"]);
    view.filters.insert(1, selected(&["1"]));
    engine.commit(&tab, engine.snapshot(&tab).unwrap().revision, view.clone()).unwrap();
    let filtered = wait(&engine, &tab);
    assert_eq!(filtered.query.unwrap().exact_total.as_deref(), Some("1"));
    assert_eq!(candidates(&engine, &tab, 1, ""), ["1", "5"], "own condition must be excluded");
    view.filters.insert(1, selected(&["1", "5"]));
    engine.commit(&tab, engine.snapshot(&tab).unwrap().revision, view.clone()).unwrap();
    assert_eq!(wait(&engine, &tab).query.unwrap().exact_total.as_deref(), Some("2"));
    view.filters.remove(&0);
    engine.commit(&tab, engine.snapshot(&tab).unwrap().revision, view.clone()).unwrap();
    assert_eq!(wait(&engine, &tab).query.unwrap().exact_total.as_deref(), Some("3"));
    engine.history(&tab, engine.snapshot(&tab).unwrap().revision, false).unwrap();
    assert_eq!(wait(&engine, &tab).query.unwrap().exact_total.as_deref(), Some("2"));
    engine.history(&tab, engine.snapshot(&tab).unwrap().revision, true).unwrap();
    let result = wait(&engine, &tab);
    let query = result.query.unwrap();
    let ipc = engine.page(&tab, &query.id, 0, 10, vec![0, 1, 2, 3]).unwrap();
    let batch = FileReader::try_new(Cursor::new(ipc), None).unwrap().next().unwrap().unwrap();
    assert_eq!(batch.num_rows(), 3);
    let cells = engine.copy_cells(&tab, &query.id, 0, 3, vec![2, 3], true).unwrap();
    assert_eq!(cells[0], ["1", "001", "a,\r\nb"]);
    let output = workspace.path().join("output.csv");
    let export = engine.export(&tab, &query.id, ExportSpec {
        path: output.clone(), columns: vec![0, 1, 2, 3], delimiter: ",".into(), encoding: "utf-8".into(), bom: false, headers: true,
    }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let state = engine.export_snapshot(&export).unwrap();
        assert_ne!(state.status, "failed", "{:?}", state.error);
        if state.status == "completed" { assert_eq!(state.rows_written, "3"); break; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let rows = csv::Reader::from_path(output).unwrap().records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(&rows[0][2], "001");
    assert_eq!(&rows[0][3], "a,\r\nb");
    let other = engine.open(source, ParseSpec::default()).unwrap();
    let reopened = wait(&engine, &other);
    assert!(reopened.cache_hit);
    assert_eq!(reopened.view, ViewState::default());
}

#[test]
fn stale_queries_and_two_tabs_cannot_override_current_state() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("data.csv");
    fs::write(&source, "A,B\nx,1\nx,5\ny,9\n").unwrap();
    let engine = setup(&workspace.path().join("cache"));
    let tab = engine.open(source.clone(), ParseSpec::default()).unwrap();
    let initial = wait(&engine, &tab);
    let old_query = initial.query.unwrap().id;
    let view = ViewState { filters: BTreeMap::from([(0, selected(&["x"]))]), sort: vec![] };
    engine.commit(&tab, initial.revision, view).unwrap();
    assert!(engine.page(&tab, &old_query, 0, 10, vec![0]).is_err());
    let filtered = wait(&engine, &tab);
    assert_eq!(filtered.query.unwrap().exact_total.as_deref(), Some("2"));
    let tab2 = engine.open(source, ParseSpec::default()).unwrap();
    assert_eq!(wait(&engine, &tab2).query.unwrap().exact_total.as_deref(), Some("3"));
    assert_eq!(engine.snapshot(&tab).unwrap().view.filters.len(), 1);
    assert!(engine.commit(&tab, 0, ViewState::default()).is_err());
}

#[test]
fn gb18030_duplicate_headers_and_long_identifiers_round_trip() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("gb18030.csv");
    let input = "名称,名称,,编号\n小区一,中文,,0012345678901234567890123\n";
    let (encoded, _, errors) = encoding_rs::GB18030.encode(input);
    assert!(!errors);
    fs::write(&source, encoded).unwrap();
    let engine = setup(&workspace.path().join("cache"));
    let tab = engine.open(source, ParseSpec { encoding: "gb18030".into(), ..Default::default() }).unwrap();
    let state = wait(&engine, &tab);
    assert_eq!(state.columns[1].name, "名称 [2]");
    assert_eq!(state.columns[2].name, "列 3");
    assert_eq!(engine.full_cell(&tab, 1, 3).unwrap(), "0012345678901234567890123");
}

#[test]
fn sorted_result_projection_and_far_jump_keep_identity_without_history_changes() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("wide.csv");
    let mut writer = csv::Writer::from_path(&source).unwrap();
    writer.write_record((0..20).map(|c| format!("c{c}"))).unwrap();
    for row in 0..1200 {
        writer.write_record((0..20).map(|column| {
            if column == 0 { (row % 3).to_string() } else { format!("{row:05}-{column}") }
        })).unwrap();
    }
    writer.flush().unwrap();
    drop(writer);
    let engine = setup(&workspace.path().join("cache"));
    let tab = engine.open(source, ParseSpec::default()).unwrap();
    let initial = wait(&engine, &tab);
    let view = ViewState {
        filters: BTreeMap::from([(0, selected(&["1"]))]),
        sort: vec![SortKey { column: 0, descending: true, interpretation: "text".into() }],
    };
    engine.commit(&tab, initial.revision, view.clone()).unwrap();
    let before = wait(&engine, &tab);
    let q = before.query.as_ref().unwrap();
    assert_eq!(q.exact_total.as_deref(), Some("400"));
    assert!(!q.projection.contains(&19));
    let old_cells = engine.copy_cells(&tab, &q.id, 300, 2, vec![0, 1], true).unwrap();
    assert_eq!(old_cells[0][0], "902", "ties retain source record order");
    engine.start_query(&tab, vec![0, 1, 19]).unwrap();
    assert_eq!(engine.snapshot(&tab).unwrap().query.unwrap().exact_total, q.exact_total);
    let projected = wait(&engine, &tab);
    let new_q = projected.query.as_ref().unwrap();
    assert_eq!(projected.revision, before.revision);
    assert_eq!(projected.view, view);
    assert_eq!(new_q.exact_total, q.exact_total);
    let new_cells = engine.copy_cells(&tab, &new_q.id, 300, 2, vec![0, 1, 19], true).unwrap();
    assert_eq!(new_cells[0][..3], old_cells[0]);
    assert_eq!(new_cells[0][3], "00901-19");
    engine.history(&tab, projected.revision, false).unwrap();
    assert_eq!(wait(&engine, &tab).view, ViewState::default());
}

#[test]
fn candidate_paging_search_rules_empty_values_and_zero_results_use_full_context() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("unique.csv");
    let mut text = "A,B\n".to_string();
    for row in 0..650 { text.push_str(&format!("x,value-{row:04}\n")); }
    text.push_str("y,\ny, \n");
    fs::write(&source, text).unwrap();
    let engine = setup(&workspace.path().join("cache"));
    let tab = engine.open(source, ParseSpec::default()).unwrap();
    let initial = wait(&engine, &tab);
    let id = engine.candidates(&tab, initial.revision, 1, "value".into()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while engine.candidate_snapshot(&id).unwrap().status != "ready" {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let first = engine.candidate_page(&id, 0).unwrap();
    assert_eq!(first.items.len(), 200);
    assert_eq!(first.exact_total.as_deref(), Some("650"));
    let second = engine.candidate_page(&id, first.next.unwrap().parse().unwrap()).unwrap();
    assert_eq!(second.items[0].label, "value-0200");
    assert_eq!(candidates(&engine, &tab, 1, "0649"), ["value-0649"]);
    let mut view = ViewState::default();
    view.filters.insert(1, ColumnFilter {
        selection: Some(ValueSelection {
            default_selected: true,
            rules: vec![SearchRule { search: "value-00".into(), selected: false }],
            overrides: vec![ValueOverride { value: FilterValue::text("value-0001"), selected: true, after_rule: 1 }],
        }), predicates: vec![],
    });
    engine.commit(&tab, initial.revision, view.clone()).unwrap();
    let state = wait(&engine, &tab);
    assert_eq!(state.query.unwrap().exact_total.as_deref(), Some("553"));
    // Explicitly selected values remain in state when another column makes them unavailable.
    view.filters.insert(0, selected(&["not-present"]));
    engine.commit(&tab, state.revision, view.clone()).unwrap();
    let zero = wait(&engine, &tab);
    assert_eq!(zero.query.unwrap().exact_total.as_deref(), Some("0"));
    assert_eq!(zero.view, view);
    assert!(candidates(&engine, &tab, 1, "").is_empty());
    let selected_value = FilterValue::text("value-0001");
    let context = engine.candidates_with_rules(&tab, zero.revision, 1, String::new(), vec![], vec![selected_value.clone()]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let candidate = engine.candidate_snapshot(&context).unwrap();
        assert_ne!(candidate.status, "failed", "{:?}", candidate.error);
        if candidate.status == "ready" {
            assert_eq!(candidate.availability[0].value, selected_value);
            assert_eq!(candidate.availability[0].available, Some(false));
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    view.filters.remove(&0);
    view.filters.insert(1, ColumnFilter { selection: None, predicates: vec![Predicate::Empty] });
    engine.commit(&tab, zero.revision, view).unwrap();
    assert_eq!(wait(&engine, &tab).query.unwrap().exact_total.as_deref(), Some("1"));
}

#[test]
fn numeric_and_datetime_interpretation_rejects_implicit_rewriting() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("types.csv");
    fs::write(&source, "number,time\n001.5,2026-01-01 00:00:00\nNaN,2026-01-01T00:00:00Z\n1e2,01/01/2026\n2.0000000001,2026-02-30\n").unwrap();
    let engine = setup(&workspace.path().join("cache"));
    let tab = engine.open(source, ParseSpec::default()).unwrap();
    let initial = wait(&engine, &tab);
    let view = ViewState {
        filters: BTreeMap::from([
            (0, ColumnFilter { selection: None, predicates: vec![Predicate::NumericRange { min: Some("1".into()), max: Some("2".into()) }] }),
            (1, ColumnFilter { selection: None, predicates: vec![Predicate::DateTimeRange { min: Some("2026-01-01".into()), max: None }] }),
        ]),
        sort: vec![],
    };
    engine.commit(&tab, initial.revision, view).unwrap();
    let result = wait(&engine, &tab);
    assert_eq!(result.query.unwrap().exact_total.as_deref(), Some("1"));
    assert_eq!(engine.full_cell(&tab, 1, 0).unwrap(), "001.5");
}

#[test]
fn closing_last_tab_releases_dataset_lease_for_cache_cleanup() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("data.csv");
    fs::write(&source, "a,b\n1,2\n").unwrap();
    let engine = setup(&workspace.path().join("cache"));
    let tab = engine.open(source, ParseSpec::default()).unwrap();
    wait(&engine, &tab);
    assert_eq!(engine.clear_unused_cache().unwrap(), 0);
    engine.close(&tab).unwrap();
    assert!(engine.snapshot(&tab).is_err());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if engine.clear_unused_cache().unwrap() > 0 { break; }
        assert!(Instant::now() < deadline, "dataset handle was not released");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn malformed_width_is_located_and_never_reported_as_complete() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("bad.csv");
    fs::write(&source, "A,B\n1,\"multiline\nvalue\"\n2\n").unwrap();
    let engine = setup(&workspace.path().join("cache"));
    let tab = engine.open(source, ParseSpec::default()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = engine.snapshot(&tab).unwrap();
        if state.status == "failed" {
            let error = state.error.unwrap();
            assert!(error.contains("期望 2 列") && error.contains("物理行"), "{error}");
            assert!(state.query.is_none());
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn long_value_references_survive_context_changes_and_searches_beyond_display_labels() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("long.csv");
    let long = "前缀".repeat(1500) + "tail-needle";
    let mut writer = csv::Writer::from_path(&source).unwrap();
    writer.write_record(["A", "B"]).unwrap();
    writer.write_record(["x", long.as_str()]).unwrap();
    writer.write_record(["y", long.as_str()]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let engine = setup(&workspace.path().join("cache"));
    let tab = engine.open(source, ParseSpec::default()).unwrap();
    let initial = wait(&engine, &tab);
    let id = engine.candidates_with_rules(&tab, initial.revision, 1, "needle".into(), vec!["needle".into()], vec![]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.candidate_snapshot(&id).unwrap().status != "ready" {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let point = engine.candidate_page(&id, 0).unwrap().items.remove(0);
    assert!(matches!(point.value, FilterValue::Reference { .. }));
    assert!(!point.label.contains("needle"));
    assert_eq!(point.matching_searches, ["needle"]);
    let view = ViewState {
        filters: BTreeMap::from([
            (0, selected(&["y"])),
            (1, ColumnFilter {
                selection: Some(ValueSelection { default_selected: false, rules: vec![],
                    overrides: vec![ValueOverride { value: point.value, selected: true, after_rule: 0 }] }),
                predicates: vec![],
            }),
        ]),
        sort: vec![],
    };
    engine.commit(&tab, initial.revision, view).unwrap();
    assert_eq!(wait(&engine, &tab).query.unwrap().exact_total.as_deref(), Some("1"));
    assert_eq!(engine.full_cell(&tab, 2, 1).unwrap(), long);
}

#[test]
fn cache_strong_verification_detects_same_metadata_content_changes() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("same.csv");
    fs::write(&source, "A,B\nx,1\n").unwrap();
    let timestamp = fs::metadata(&source).unwrap().modified().unwrap();
    let engine = setup(&workspace.path().join("cache"));
    let first = engine.open(source.clone(), ParseSpec::default()).unwrap();
    wait(&engine, &first);
    engine.close(&first).unwrap();
    fs::write(&source, "A,B\ny,2\n").unwrap();
    File::options().write(true).open(&source).unwrap().set_modified(timestamp).unwrap();
    let second = engine.open(source, ParseSpec::default()).unwrap();
    let state = wait(&engine, &second);
    assert!(!state.cache_hit, "same-size same-mtime content must not use stale cache");
    assert_eq!(engine.full_cell(&second, 1, 0).unwrap(), "y");
}

#[test]
fn configured_tab_limit_is_global_and_closing_a_tab_returns_capacity() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("file.csv");
    fs::write(&source, "A,B\nx,1\n").unwrap();
    let mut config = EngineConfig::new(workspace.path().join("cache"));
    config.reserve_bytes = 0;
    config.memory_limit_mb = 256;
    config.max_sessions = 2;
    let engine = Engine::new(config).unwrap();
    let first = engine.open(source.clone(), ParseSpec::default()).unwrap();
    let second = engine.open(source.clone(), ParseSpec::default()).unwrap();
    assert!(engine.open(source.clone(), ParseSpec::default()).is_err());
    wait(&engine, &first);
    wait(&engine, &second);
    engine.close(&first).unwrap();
    let third = engine.open(source, ParseSpec::default()).unwrap();
    assert_eq!(wait(&engine, &third).query.unwrap().exact_total.as_deref(), Some("1"));
}
