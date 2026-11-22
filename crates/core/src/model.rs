use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_FIELD_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_COLUMNS: usize = 2048;
pub const PREVIEW_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct ParseSpec {
    pub encoding: String,
    pub delimiter: Option<String>,
    pub has_headers: bool,
    pub max_record_bytes: usize,
    pub max_field_bytes: usize,
    pub max_columns: usize,
}

impl Default for ParseSpec {
    fn default() -> Self {
        Self {
            encoding: "auto".into(),
            delimiter: None,
            has_headers: true,
            max_record_bytes: MAX_RECORD_BYTES,
            max_field_bytes: MAX_FIELD_BYTES,
            max_columns: MAX_COLUMNS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub id: usize,
    pub name: String,
    pub original_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum FilterValue {
    Text { text: String },
    Reference {
        rid: String, column: usize,
        #[serde(default)] digest: String,
        #[serde(default)] byte_length: u64,
    },
}

impl FilterValue {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ValueOverride {
    pub value: FilterValue,
    pub selected: bool,
    #[serde(default)]
    pub after_rule: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchRule {
    pub search: String,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ValueSelection {
    pub default_selected: bool,
    pub rules: Vec<SearchRule>,
    pub overrides: Vec<ValueOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Predicate {
    Equals { text: String },
    NotEquals { text: String },
    Contains { text: String },
    NotContains { text: String },
    Empty,
    NotEmpty,
    NumericRange { min: Option<String>, max: Option<String> },
    DateTimeRange { min: Option<String>, max: Option<String> },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ColumnFilter {
    pub selection: Option<ValueSelection>,
    pub predicates: Vec<Predicate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SortKey {
    pub column: usize,
    pub descending: bool,
    #[serde(default = "text_sort")]
    pub interpretation: String,
}
fn text_sort() -> String { "text".into() }

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EditState {
    #[serde(default)]
    pub cells: BTreeMap<u64, BTreeMap<usize, String>>,
    #[serde(default, with = "record_ids")]
    pub deleted_rows: BTreeSet<u64>,
    #[serde(default)]
    pub deleted_columns: BTreeSet<usize>,
    #[serde(default)]
    pub colors: BTreeMap<u64, BTreeMap<usize, String>>,
}

impl EditState {
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.deleted_rows.is_empty()
            && self.deleted_columns.is_empty() && self.colors.is_empty()
    }
    pub fn same_data(&self, other: &Self) -> bool {
        self.cells == other.cells && self.deleted_rows == other.deleted_rows
            && self.deleted_columns == other.deleted_columns
    }
}

mod record_ids {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
    use std::collections::BTreeSet;

    pub fn serialize<S: Serializer>(values: &BTreeSet<u64>, serializer: S) -> Result<S::Ok, S::Error> {
        values.iter().map(u64::to_string).collect::<Vec<_>>().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<BTreeSet<u64>, D::Error> {
        Vec::<String>::deserialize(deserializer)?.into_iter()
            .map(|value| value.parse().map_err(D::Error::custom)).collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ViewState {
    pub filters: BTreeMap<usize, ColumnFilter>,
    pub sort: Vec<SortKey>,
    #[serde(default, skip_serializing_if = "EditState::is_empty")]
    pub edits: EditState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuerySnapshot {
    pub id: String,
    pub revision: u64,
    pub status: String,
    pub generated_rows: String,
    pub exact_total: Option<String>,
    pub projection: Vec<usize>,
    pub error: Option<String>,
    pub scanned_rows: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub status: String,
    pub phase: String,
    pub bytes_read: String,
    pub total_bytes: String,
    pub rows_read: String,
    pub columns: Vec<Column>,
    pub preview: Vec<Vec<String>>,
    pub parse_spec: ParseSpec,
    pub warnings: Vec<String>,
    pub error: Option<String>,
    pub revision: u64,
    pub view: ViewState,
    pub can_undo: bool,
    pub can_redo: bool,
    pub query: Option<QuerySnapshot>,
    pub cache_hit: bool,
    pub modified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub value: FilterValue,
    pub label: String,
    pub empty: bool,
    pub matching_searches: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateSnapshot {
    pub id: String,
    pub tab_id: String,
    pub revision: u64,
    pub column: usize,
    pub search: String,
    pub status: String,
    pub count: String,
    pub error: Option<String>,
    pub availability: Vec<ValueAvailability>,
    pub partial: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueAvailability {
    pub value: FilterValue,
    pub available: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidatePage {
    pub id: String,
    pub revision: u64,
    pub items: Vec<Candidate>,
    pub next: Option<String>,
    pub exact_total: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSpec {
    pub path: PathBuf,
    pub columns: Vec<usize>,
    pub delimiter: String,
    pub encoding: String,
    pub bom: bool,
    pub headers: bool,
    #[serde(default)]
    pub all_rows: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSnapshot {
    pub id: String,
    pub tab_id: String,
    pub status: String,
    pub path: PathBuf,
    pub rows_written: String,
    pub total_rows: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub cache_dir: PathBuf,
    pub memory_limit_mb: usize,
    pub threads: usize,
    pub cache_limit_bytes: u64,
    pub result_limit_bytes: u64,
    pub temp_limit_bytes: u64,
    pub reserve_bytes: u64,
    pub max_sessions: usize,
}

impl EngineConfig {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            memory_limit_mb: 2048,
            threads: std::thread::available_parallelism().map(|n| n.get().min(4)).unwrap_or(2),
            cache_limit_bytes: 64 * 1024 * 1024 * 1024,
            result_limit_bytes: 16 * 1024 * 1024 * 1024,
            temp_limit_bytes: 16 * 1024 * 1024 * 1024,
            reserve_bytes: 4 * 1024 * 1024 * 1024,
            max_sessions: 16,
        }
    }
}
