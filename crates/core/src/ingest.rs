use crate::parser::{CsvStream, ReadOutcome, validate_width};
use anyhow::{Result, ensure};
use duckdb::arrow::{
    array::{ArrayRef, StringBuilder, UInt64Array},
    datatypes::{DataType, Field, Schema},
    record_batch::RecordBatch,
};
use std::{sync::{Arc, Condvar, Mutex, atomic::{AtomicBool, Ordering}, mpsc::SyncSender}, time::Duration};

const BATCH_BYTES: usize = 8 * 1024 * 1024;

/// Shared admission for Arrow buffers across all import pipelines.
pub struct BufferBudget {
    used: Mutex<usize>,
    wake: Condvar,
}
struct BufferPermit { budget: Arc<BufferBudget>, bytes: usize }
impl BufferBudget {
    pub fn new() -> Arc<Self> { Arc::new(Self { used: Mutex::new(0), wake: Condvar::new() }) }
    fn acquire(self: &Arc<Self>, bytes: usize, cancel: &AtomicBool) -> Result<BufferPermit> {
        let bytes = bytes.min(128 * 1024 * 1024);
        let mut used = self.used.lock().unwrap();
        loop {
            ensure!(!cancel.load(Ordering::Relaxed), "任务已取消");
            if *used + bytes <= 128 * 1024 * 1024 {
                *used += bytes;
                return Ok(BufferPermit { budget: self.clone(), bytes });
            }
            used = self.wake.wait_timeout(used, Duration::from_millis(20)).unwrap().0;
        }
    }
}
impl Drop for BufferPermit {
    fn drop(&mut self) {
        *self.budget.used.lock().unwrap() -= self.bytes;
        self.budget.wake.notify_all();
    }
}

pub struct ImportBatch {
    pub records: RecordBatch,
    pub bytes_read: u64,
    _permit: BufferPermit,
}
pub struct Parsed {
    pub rows: u64,
    pub hash: String,
}

/// A single producer owns the CSV state machine, including quoted newlines and
/// strict decoding. One queued batch overlaps parsing with database insertion.
pub fn produce(
    mut stream: CsvStream, preview: Vec<Vec<String>>, width: usize, mut eof: bool,
    cancel: &AtomicBool, sender: SyncSender<ImportBatch>, budget: Arc<BufferBudget>,
) -> Result<Parsed> {
    // Reserve before allocating. The estimate includes a maximum-size record
    // beyond the flush threshold and geometric Arrow buffer growth. A user-
    // configured record budget above 56MiB is admitted alone.
    let reservation = 2 * (BATCH_BYTES + stream.spec().max_record_bytes + width * 16);
    let mut permit = Some(budget.acquire(reservation, cancel)?);
    let mut count = 0u64;
    let mut buffered = preview.len();
    let mut bytes = preview.iter().flatten().map(String::len).sum::<usize>();
    let mut builders: Vec<_> = (0..width).map(|_| StringBuilder::new()).collect();
    for row in preview {
        for (builder, value) in builders.iter_mut().zip(row) { builder.append_value(value); }
    }
    let mut fields = vec![Field::new("_source_rid", DataType::UInt64, false)];
    fields.extend((0..width).map(|c| Field::new(format!("c{c}"), DataType::Utf8, false)));
    let schema = Arc::new(Schema::new(fields));
    loop {
        // Include Arrow offsets and identities in the limit, especially for wide
        // files containing many empty/small fields.
        let estimated = bytes.saturating_add(buffered.saturating_mul(width * 4 + 8));
        if buffered > 0 && (count == 0 || estimated >= BATCH_BYTES || buffered >= 32768 || eof) {
            let first = count + 1;
            let mut arrays: Vec<ArrayRef> = vec![Arc::new(UInt64Array::from_iter_values(first..first + buffered as u64))];
            arrays.extend(builders.iter_mut().map(|b| Arc::new(b.finish()) as ArrayRef));
            let records = RecordBatch::try_new(schema.clone(), arrays)?;
            ensure!(sender.send(ImportBatch { records, bytes_read: stream.bytes_read, _permit: permit.take().unwrap() }).is_ok(), "导入已结束");
            count += buffered as u64;
            buffered = 0;
            bytes = 0;
            if !eof { permit = Some(budget.acquire(reservation, cancel)?); }
        }
        if eof { break; }
        match stream.next_into(None, cancel, |column, value| {
            if let Some(builder) = builders.get_mut(column) { builder.append_value(value); }
            bytes += value.len();
        })? {
            ReadOutcome::Record(record) => {
                validate_width(&record, width, count + buffered as u64 + 1)?;
                buffered += 1;
            }
            ReadOutcome::Yield => {}
            ReadOutcome::End => eof = true,
        }
    }
    Ok(Parsed { rows: count, hash: stream.hash() })
}
