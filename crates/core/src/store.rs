use anyhow::{Context, Result, ensure};
use duckdb::arrow::{
    array::{ArrayRef, StringArray, UInt64Array},
    compute::concat_batches,
    datatypes::{DataType, Field, Schema},
    ipc::{reader::FileReader, writer::FileWriter},
    record_batch::RecordBatch,
    util::display::array_value_to_string,
};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Serialize, Deserialize)]
pub struct Fragment {
    pub file: String,
    pub start: u64,
    pub rows: u64,
}

#[derive(Default)]
struct Index {
    fragments: Vec<Fragment>,
    generated: u64,
    complete: bool,
}

pub struct ResultStore {
    pub directory: PathBuf,
    pub columns: Vec<usize>,
    index: Mutex<Index>,
}

pub struct StoreWriter {
    store: std::sync::Arc<ResultStore>,
    writer: Option<FileWriter<File>>,
    rid_file: File,
    current_name: String,
    current_rows: u64,
    current_bytes: usize,
    total_rows: u64,
    sequence: usize,
    disk_limit: u64,
    bytes_written: u64,
}

impl ResultStore {
    pub fn create(directory: PathBuf, columns: Vec<usize>) -> Result<std::sync::Arc<Self>> {
        fs::create_dir_all(&directory)?;
        Ok(std::sync::Arc::new(Self { directory, columns, index: Mutex::new(Index::default()) }))
    }
    pub fn generated(&self) -> u64 { self.index.lock().unwrap().generated }
    pub fn complete(&self) -> bool { self.index.lock().unwrap().complete }
    pub fn identity(&self, count: u64) {
        let mut index = self.index.lock().unwrap();
        index.generated = count;
        index.complete = true;
    }
    pub fn writer(self: &std::sync::Arc<Self>, disk_limit: u64) -> Result<StoreWriter> {
        let rid_file = File::create(self.directory.join("rids.bin"))?;
        Ok(StoreWriter {
            store: self.clone(), writer: None, rid_file, current_name: String::new(),
            current_rows: 0, current_bytes: 0, total_rows: 0, sequence: 0,
            disk_limit, bytes_written: 0,
        })
    }
    pub fn read(&self, start: u64, count: usize) -> Result<RecordBatch> {
        ensure!(count <= 2048 && count > 0, "页大小无效");
        let index = self.index.lock().unwrap();
        ensure!(start < index.generated, if index.complete { "结果位置越界" } else { "Pending：结果尚未生成" });
        let end = (start + count as u64).min(index.generated);
        let first = index.fragments.partition_point(|f| f.start + f.rows <= start);
        let fragments: Vec<_> = index.fragments[first..].iter().take_while(|f| f.start < end).cloned().collect();
        drop(index);
        let mut batches = vec![];
        for fragment in fragments {
            let reader = FileReader::try_new(File::open(self.directory.join(&fragment.file))?, None)?;
            let mut position = fragment.start;
            for batch in reader {
                let batch = batch?;
                let finish = position + batch.num_rows() as u64;
                if finish > start && position < end {
                    let offset = start.saturating_sub(position) as usize;
                    let length = ((end.min(finish) - position) as usize).saturating_sub(offset);
                    if length > 0 { batches.push(batch.slice(offset, length)); }
                }
                position = finish;
                if position >= end { break; }
            }
        }
        let schema = batches.first().context("结果页未就绪")?.schema();
        Ok(concat_batches(&schema, &batches)?)
    }
    pub fn rid_at(&self, position: u64) -> Result<u64> {
        ensure!(position < self.generated(), "结果位置尚未生成");
        let mut file = File::open(self.directory.join("rids.bin"))?;
        file.seek(SeekFrom::Start(position * 8))?;
        let mut bytes = [0; 8];
        file.read_exact(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }
    pub fn rid_file(&self) -> PathBuf { self.directory.join("rids.bin") }
}

impl Drop for ResultStore {
    fn drop(&mut self) {
        if self.directory.file_name().and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("query-") || s.starts_with("candidate-")) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

impl StoreWriter {
    pub fn append(&mut self, batch: &RecordBatch) -> Result<bool> {
        if batch.num_rows() == 0 { return Ok(false); }
        let bytes = batch.get_array_memory_size();
        ensure!(self.bytes_written.saturating_add(bytes as u64) <= self.disk_limit,
            "结果缓存超过配额，请减少可见列或提高结果缓存预算");
        if self.writer.is_none() {
            self.current_name = format!("part-{:06}.arrow", self.sequence);
            let file = File::create(self.store.directory.join(&self.current_name))?;
            self.writer = Some(FileWriter::try_new(file, &batch.schema())?);
        }
        self.writer.as_mut().unwrap().write(batch)?;
        let ids = batch.column(0).as_any().downcast_ref::<UInt64Array>().context("结果缺少稳定记录序号")?;
        let mut id_buffer = Vec::with_capacity(ids.len() * 8);
        for id in ids.values() { id_buffer.extend_from_slice(&id.to_le_bytes()); }
        self.rid_file.write_all(&id_buffer)?;
        self.current_rows += batch.num_rows() as u64;
        self.current_bytes += bytes;
        self.bytes_written += bytes as u64 + id_buffer.len() as u64;
        if self.sequence == 0 || self.current_bytes >= 4 * 1024 * 1024 {
            self.seal()?;
            return Ok(true);
        }
        Ok(false)
    }
    fn seal(&mut self) -> Result<()> {
        if let Some(mut writer) = self.writer.take() {
            writer.finish()?;
            self.rid_file.flush()?;
            let mut index = self.store.index.lock().unwrap();
            index.fragments.push(Fragment { file: self.current_name.clone(), start: self.total_rows, rows: self.current_rows });
            self.total_rows += self.current_rows;
            index.generated = self.total_rows;
            self.sequence += 1;
            self.current_rows = 0;
            self.current_bytes = 0;
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<u64> {
        self.seal()?;
        self.rid_file.sync_all()?;
        let mut index = self.store.index.lock().unwrap();
        index.complete = true;
        fs::write(self.store.directory.join("manifest.json"), serde_json::to_vec(&index.fragments)?)?;
        Ok(self.total_rows)
    }
}

pub fn to_ipc(batch: &RecordBatch) -> Result<Vec<u8>> {
    let mut bytes = vec![];
    {
        let mut writer = FileWriter::try_new(&mut bytes, &batch.schema())?;
        writer.write(batch)?;
        writer.finish()?;
    }
    Ok(bytes)
}

pub fn display_batch(batch: &RecordBatch, columns: &[usize]) -> Result<RecordBatch> {
    let mut arrays: Vec<ArrayRef> = vec![batch.column(0).clone()];
    let mut fields = vec![Field::new("_source_rid", DataType::UInt64, false)];
    let mut remaining = 2 * 1024 * 1024;
    for &column in columns {
        let name = format!("c{column}");
        let index = batch.schema().index_of(&name)?;
        let source = batch.column(index);
        let mut values = Vec::with_capacity(batch.num_rows());
        for row in 0..batch.num_rows() {
            let value = array_value_to_string(source.as_ref(), row)?;
            let text: String = value.chars().take(1024).collect();
            if text.len() <= remaining {
                remaining -= text.len();
                values.push(text);
            } else {
                values.push("…".into());
            }
        }
        arrays.push(std::sync::Arc::new(StringArray::from(values)));
        fields.push(Field::new(name, DataType::Utf8, false));
    }
    Ok(RecordBatch::try_new(std::sync::Arc::new(Schema::new(fields)), arrays)?)
}

pub fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else { return 0 };
    entries.filter_map(|e| e.ok()).map(|e| {
        let Ok(metadata) = e.metadata() else { return 0 };
        if metadata.is_dir() { directory_size(&e.path()) } else { metadata.len() }
    }).sum()
}

pub fn write_new_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.sync_all()?;
    Ok(())
}
