use crate::model::*;
use anyhow::{Result, bail, ensure};

pub struct Compiled {
    pub sql: String,
    pub parameters: Vec<String>,
}

pub fn column_name(column: usize, count: usize) -> Result<String> {
    ensure!(column < count, "列标识无效：{column}");
    Ok(format!("c{column}"))
}

pub fn numeric_expression(column: &str) -> String {
    // Cast alone rounds excessive fractional digits: validate before converting.
    format!("CASE WHEN regexp_full_match({column}, '[+-]?[0-9]{{1,29}}(\\.[0-9]{{1,9}})?') THEN try_cast({column} AS DECIMAL(38,9)) ELSE NULL END")
}

pub fn datetime_expression(column: &str) -> String {
    format!("CASE WHEN regexp_full_match({column}, '[0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}([ T][0-9]{{2}}:[0-9]{{2}}:[0-9]{{2}}(\\.[0-9]{{1,6}})?)?') THEN try_cast({column} AS TIMESTAMP) ELSE NULL END")
}

fn validate_datetime(value: &str) -> Result<()> {
    let b = value.as_bytes();
    let date = b.len() >= 10 && b[4] == b'-' && b[7] == b'-'
        && b[..4].iter().chain(&b[5..7]).chain(&b[8..10]).all(u8::is_ascii_digit);
    let time = b.len() >= 19 && matches!(b[10], b' ' | b'T') && b[13] == b':' && b[16] == b':'
        && b[11..13].iter().chain(&b[14..16]).chain(&b[17..19]).all(u8::is_ascii_digit);
    let fraction = (21..=26).contains(&b.len()) && b[19] == b'.' && b[20..].iter().all(u8::is_ascii_digit);
    ensure!(date && (b.len() == 10 || (time && (b.len() == 19 || fraction))),
        "日期应为 YYYY-MM-DD 或 YYYY-MM-DD HH:MM:SS[.ffffff]，不含时区");
    Ok(())
}

pub fn compile_filters(
    view: &ViewState,
    excluded: Option<usize>,
    count: usize,
    mut resolve: impl FnMut(&FilterValue) -> Result<String>,
) -> Result<Compiled> {
    let mut columns = vec![];
    let mut parameters = vec![];
    for (&id, filter) in &view.filters {
        if Some(id) == excluded { continue; }
        let c = column_name(id, count)?;
        let mut parts = vec![];
        if let Some(selection) = &filter.selection {
            ensure!(selection.rules.len() <= 64 && selection.overrides.len() <= 10_000, "筛选选择过于复杂，请使用条件规则");
            let mut expression = if selection.default_selected { "TRUE" } else { "FALSE" }.to_string();
            let mut local = vec![];
            let mut bytes = parameters.iter().map(String::len).sum::<usize>();
            for stage in 0..=selection.rules.len() {
                let mut points = std::collections::BTreeMap::new();
                for point in selection.overrides.iter().filter(|p| p.after_rule == stage) {
                    let value = resolve(&point.value)?;
                    bytes += value.len();
                    ensure!(bytes <= 8 * 1024 * 1024, "完整筛选参数超过 8MiB，请减少巨型值勾选或使用文本条件");
                    points.insert(value, point.selected);
                }
                let mut cases = vec![];
                let mut stage_parameters = vec![];
                for selected in [true, false] {
                    let values: Vec<_> = points.iter().filter(|(_, yes)| **yes == selected).map(|(s, _)| s.clone()).collect();
                    if !values.is_empty() {
                        cases.push(format!("WHEN {c} IN ({}) THEN {selected}", vec!["?"; values.len()].join(",")));
                        stage_parameters.extend(values);
                    }
                }
                if !cases.is_empty() {
                    expression = format!("CASE {} ELSE ({expression}) END", cases.join(" "));
                    stage_parameters.extend(local);
                    local = stage_parameters;
                }
                if let Some(rule) = selection.rules.get(stage) {
                    expression = format!("CASE WHEN contains({c}, ?) THEN {} ELSE ({expression}) END", rule.selected);
                    local.insert(0, rule.search.clone());
                }
            }
            ensure!(selection.overrides.iter().all(|p| p.after_rule <= selection.rules.len()), "无效的选择动作顺序");
            parameters.extend(local);
            parts.push(expression);
        }
        for predicate in &filter.predicates {
            match predicate {
                Predicate::Equals { text } => { parts.push(format!("{c} = ?")); parameters.push(text.clone()); }
                Predicate::NotEquals { text } => { parts.push(format!("{c} <> ?")); parameters.push(text.clone()); }
                Predicate::Contains { text } => { parts.push(format!("contains({c}, ?)")); parameters.push(text.clone()); }
                Predicate::NotContains { text } => { parts.push(format!("NOT contains({c}, ?)")); parameters.push(text.clone()); }
                Predicate::Empty => parts.push(format!("{c} = ''")),
                Predicate::NotEmpty => parts.push(format!("{c} <> ''")),
                Predicate::NumericRange { min, max } => {
                    let expression = numeric_expression(&c);
                    for (bound, op) in [(min, ">="), (max, "<=")] {
                        if let Some(bound) = bound {
                            validate_decimal(bound)?;
                            parts.push(format!("({expression}) {op} CAST(? AS DECIMAL(38,9))"));
                            parameters.push(bound.clone());
                        }
                    }
                    parts.push(format!("({expression}) IS NOT NULL"));
                }
                Predicate::DateTimeRange { min, max } => {
                    let expression = datetime_expression(&c);
                    for (bound, op) in [(min, ">="), (max, "<=")] {
                        if let Some(bound) = bound {
                            validate_datetime(bound)?;
                            parts.push(format!("({expression}) {op} CAST(? AS TIMESTAMP)"));
                            parameters.push(bound.clone());
                        }
                    }
                    parts.push(format!("({expression}) IS NOT NULL"));
                }
            }
        }
        if !parts.is_empty() { columns.push(format!("({})", parts.join(" AND "))); }
        ensure!(parameters.iter().map(String::len).sum::<usize>() <= 8 * 1024 * 1024, "筛选参数超过 8MiB");
    }
    Ok(Compiled { sql: if columns.is_empty() { "TRUE".into() } else { columns.join(" AND ") }, parameters })
}

pub fn compile_order(view: &ViewState, count: usize) -> Result<String> {
    let mut result = vec![];
    ensure!(view.sort.len() <= 8, "首版最多支持 8 个排序键");
    for sort in &view.sort {
        let c = column_name(sort.column, count)?;
        let expression = match sort.interpretation.as_str() {
            "text" => c,
            "number" => numeric_expression(&c),
            "datetime" => datetime_expression(&c),
            _ => bail!("未知排序解释方式"),
        };
        result.push(format!("{expression} {} NULLS LAST", if sort.descending { "DESC" } else { "ASC" }));
    }
    result.push("_source_rid ASC".into());
    Ok(result.join(", "))
}

pub fn validate_decimal(s: &str) -> Result<()> {
    let number = s.strip_prefix(['+', '-']).unwrap_or(s);
    let parts: Vec<_> = number.split('.').collect();
    ensure!(parts.len() <= 2 && !parts[0].is_empty() && parts[0].len() <= 29
        && parts[0].bytes().all(|b| b.is_ascii_digit())
        && (parts.len() == 1 || (!parts[1].is_empty() && parts[1].len() <= 9 && parts[1].bytes().all(|b| b.is_ascii_digit()))),
        "数值必须可精确表示为 DECIMAL(38,9)：{s}");
    Ok(())
}
