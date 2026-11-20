use crate::model::{EditState, MAX_FIELD_BYTES};
use anyhow::{Result, ensure};
use std::collections::BTreeMap;

/// Sparse edits belong to a session/history node, never to the shared source cache.
pub fn validate(edits: &EditState, width: usize, rows: u64, field_limit: usize) -> Result<()> {
    ensure!(edits.deleted_columns.len() < width, "至少保留一列数据");
    for &column in &edits.deleted_columns {
        ensure!(column < width, "删除列超出范围");
    }
    for &rid in &edits.deleted_rows {
        ensure!((1..=rows).contains(&rid), "删除行超出源记录范围");
    }
    for (&rid, values) in &edits.cells {
        ensure!((1..=rows).contains(&rid), "编辑行超出源记录范围");
        for (&column, value) in values {
            ensure!(column < width, "编辑列超出范围");
            ensure!(value.len() <= field_limit.min(MAX_FIELD_BYTES), "编辑内容超过字段字节上限");
        }
    }
    for (&rid, colors) in &edits.colors {
        ensure!((1..=rows).contains(&rid), "标记行超出源记录范围");
        for (&column, color) in colors {
            ensure!(column < width, "标记列超出范围");
            ensure!(valid_color(color), "颜色必须是 #RRGGBB 格式");
        }
    }
    Ok(())
}

pub fn valid_color(color: &str) -> bool {
    color.len() == 7 && color.starts_with('#') && color[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

fn text_literal(value: &str) -> String {
    if value.contains('\0') {
        let hex = value.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
        format!("decode(from_hex('{hex}'))")
    } else {
        format!("'{}'", value.replace('\'', "''"))
    }
}

/// Preserve the source relation unchanged in the common, unedited case.
/// Small VALUES joins let DuckDB push record-range predicates into its base table.
pub fn source(alias: &str, width: usize, edits: &EditState) -> String {
    if edits.cells.is_empty() && edits.deleted_rows.is_empty() {
        return format!("{alias}.data");
    }
    let mut by_column: BTreeMap<usize, Vec<(u64, &str)>> = BTreeMap::new();
    for (&rid, cells) in &edits.cells {
        for (&column, value) in cells {
            by_column.entry(column).or_default().push((rid, value));
        }
    }
    let mut selected = vec!["d._source_rid".to_string()];
    let mut joins = String::new();
    for column in 0..width {
        if let Some(values) = by_column.get(&column) {
            let values = values.iter().map(|(rid, value)| format!("({rid},{})", text_literal(value)))
                .collect::<Vec<_>>().join(",");
            selected.push(format!("coalesce(e{column}.value, d.c{column}) AS c{column}"));
            joins.push_str(&format!(" LEFT JOIN (VALUES {values}) e{column}(rid,value) ON d._source_rid=e{column}.rid"));
        } else {
            selected.push(format!("d.c{column}"));
        }
    }
    let removed = if edits.deleted_rows.is_empty() { String::new() } else {
        format!(" WHERE d._source_rid NOT IN ({})",
            edits.deleted_rows.iter().map(u64::to_string).collect::<Vec<_>>().join(","))
    };
    format!("(SELECT {} FROM {alias}.data d{joins}{removed})", selected.join(","))
}
