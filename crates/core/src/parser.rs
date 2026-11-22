use crate::model::*;
use anyhow::{Context, Result, bail, ensure};
use csv_core::ReadRecordResult;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct CsvRecord {
    pub fields: Vec<String>,
    pub field_count: usize,
    pub byte_start: u64,
    pub line_start: u64,
}
pub enum ReadOutcome { Record(CsvRecord), Yield, End }

pub struct CsvStream {
    file: File,
    reader: csv_core::Reader,
    spec: ParseSpec,
    input: Vec<u8>,
    input_pos: usize,
    input_len: usize,
    output: Vec<u8>,
    output_len: usize,
    ends: Vec<usize>,
    ends_len: usize,
    eof: bool,
    bom: usize,
    pub consumed: u64,
    pub bytes_read: u64,
    pub logical_records: u64,
    line: u64,
    previous_cr: bool,
    record_start: u64,
    record_line: u64,
    hasher: blake3::Hasher,
}

pub fn stable_open(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)] {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ; deny write/delete while preparing.
    }
    options.open(path).context("无法取得稳定只读快照；请关闭正在写入该文件的程序")
}

impl CsvStream {
    pub fn open(path: &Path, mut spec: ParseSpec) -> Result<(Self, Vec<String>)> {
        ensure!(spec.max_record_bytes > 0 && spec.max_record_bytes <= 128 * 1024 * 1024, "记录预算无效");
        ensure!(spec.max_field_bytes > 0 && spec.max_field_bytes <= 128 * 1024 * 1024, "字段预算无效");
        ensure!(spec.max_columns > 0 && spec.max_columns <= 16384, "列数预算无效");
        let mut file = stable_open(path)?;
        let mut sample = vec![0; 65536];
        let len = file.read(&mut sample)?;
        sample.truncate(len);
        file.seek(SeekFrom::Start(0))?;
        let mut warnings = vec![];
        let bom = if sample.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 };
        if spec.encoding == "auto" {
            let valid = match std::str::from_utf8(&sample[bom..]) {
                Ok(_) => true,
                Err(e) => e.error_len().is_none(),
            };
            spec.encoding = if bom > 0 || valid { "utf-8" } else { "gb18030" }.into();
            if !valid { warnings.push("编码推测为 GB18030；请在解析设置中核对。".into()); }
        }
        ensure!(matches!(spec.encoding.as_str(), "utf-8" | "gbk" | "gb18030"), "不支持的编码");
        let delimiter = if let Some(d) = &spec.delimiter {
            ensure!(d.len() == 1 && matches!(d.as_bytes()[0], b',' | b';' | b'\t' | b'|'), "分隔符必须为逗号、分号、Tab 或竖线");
            d.as_bytes()[0]
        } else {
            let mut best = (0usize, b',');
            for d in [b',', b';', b'\t', b'|'] {
                if d == b'|' && spec.encoding != "utf-8" { continue; }
                let mut r = csv::ReaderBuilder::new().delimiter(d).has_headers(false).flexible(true).from_reader(&sample[bom..]);
                let lengths: Vec<_> = r.byte_records().take(12).filter_map(|r| r.ok()).map(|r| r.len()).collect();
                let score = lengths.first().filter(|n| **n > 1)
                    .map(|n| lengths.iter().filter(|m| *m == n).count() * 100 + (*n).min(90)).unwrap_or(0);
                if score > best.0 { best = (score, d); }
            }
            best.1
        };
        ensure!(delimiter != b'|' || spec.encoding == "utf-8",
            "GBK/GB18030 的多字节字符可能包含竖线字节，请选择逗号、分号或 Tab");
        spec.delimiter = Some((delimiter as char).to_string());
        let reader = csv_core::ReaderBuilder::new().delimiter(delimiter).build();
        Ok((Self {
            file, reader, spec, input: vec![0; 65536], input_pos: 0, input_len: 0,
            output: vec![0; 8192], output_len: 0, ends: vec![0; 64], ends_len: 0,
            eof: false, bom, consumed: 0, bytes_read: 0, logical_records: 0,
            line: 1, previous_cr: false, record_start: bom as u64, record_line: 1,
            hasher: blake3::Hasher::new(),
        }, warnings))
    }
    pub fn spec(&self) -> &ParseSpec { &self.spec }
    /// Retain the stable read lease when parsing finishes before ingestion.
    pub fn source_guard(&self) -> Result<File> { Ok(self.file.try_clone()?) }
    pub fn hash(&self) -> String { self.hasher.finalize().to_hex().to_string() }
    pub fn finish_hash(&mut self, cancel: &AtomicBool, mut progress: impl FnMut(u64)) -> Result<String> {
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            ensure!(!cancel.load(Ordering::Relaxed), "任务已取消");
            let n = self.file.read(&mut buffer)?;
            if n == 0 { break; }
            self.hasher.update(&buffer[..n]);
            self.bytes_read += n as u64;
            progress(self.bytes_read);
        }
        Ok(self.hash())
    }
    pub fn next(&mut self, budget: Option<u64>, cancel: &AtomicBool) -> Result<ReadOutcome> {
        let mut fields = Vec::new();
        let mut outcome = self.next_into(budget, cancel, |_, value| fields.push(value.to_owned()))?;
        if let ReadOutcome::Record(record) = &mut outcome { record.fields = fields; }
        Ok(outcome)
    }
    /// Deliver borrowed decoded fields directly to a batch builder. The owned
    /// record API above is still used for the small preview and parser clients.
    pub fn next_into(&mut self, budget: Option<u64>, cancel: &AtomicBool, mut field: impl FnMut(usize, &str)) -> Result<ReadOutcome> {
        let begin = self.consumed;
        loop {
            ensure!(!cancel.load(Ordering::Relaxed), "任务已取消");
            if budget.is_some_and(|b| self.consumed - begin >= b) { return Ok(ReadOutcome::Yield); }
            if self.input_pos == self.input_len && !self.eof {
                let n = self.file.read(&mut self.input)?;
                self.input_len = n;
                self.input_pos = 0;
                self.eof = n == 0;
                if n > 0 {
                    self.hasher.update(&self.input[..n]);
                    self.bytes_read += n as u64;
                    if self.bytes_read == n as u64 && self.bom > 0 {
                        self.input_pos = self.bom;
                        self.consumed = self.bom as u64;
                    }
                }
            }
            let remaining = budget.map(|b| b.saturating_sub(self.consumed - begin) as usize).unwrap_or(usize::MAX);
            let end = self.input_len.min(self.input_pos.saturating_add(remaining));
            let input = &self.input[self.input_pos..end];
            // Do not pass empty input unless this is actual EOF: empty means EOF to csv-core.
            if input.is_empty() && !self.eof { continue; }
            let (outcome, used, written, fields) = self.reader.read_record(
                input, &mut self.output[self.output_len..], &mut self.ends[self.ends_len..]);
            for &b in &input[..used] {
                if b == b'\r' { self.line += 1; self.previous_cr = true; }
                else if b == b'\n' {
                    if !self.previous_cr { self.line += 1; }
                    self.previous_cr = false;
                } else { self.previous_cr = false; }
            }
            self.input_pos += used;
            self.consumed += used as u64;
            self.output_len += written;
            self.ends_len += fields;
            ensure!(self.consumed - self.record_start <= self.spec.max_record_bytes as u64,
                "逻辑记录 {}，字节 {}，物理行 {}：原始记录超过 {} 字节",
                self.logical_records + 1, self.record_start, self.record_line, self.spec.max_record_bytes);
            match outcome {
                ReadRecordResult::InputEmpty => {}
                ReadRecordResult::OutputFull => {
                    ensure!(self.output.len() < self.spec.max_record_bytes, "逻辑记录 {}：解码前字段数据超限", self.logical_records + 1);
                    self.output.resize((self.output.len() * 2).min(self.spec.max_record_bytes), 0);
                }
                ReadRecordResult::OutputEndsFull => {
                    ensure!(self.ends.len() < self.spec.max_columns, "逻辑记录 {}：列数超过上限 {}", self.logical_records + 1, self.spec.max_columns);
                    self.ends.resize((self.ends.len() * 2).min(self.spec.max_columns), 0);
                }
                ReadRecordResult::Record => {
                    ensure!(self.ends_len <= self.spec.max_columns, "逻辑记录 {}：列数超过上限", self.logical_records + 1);
                    let mut start = 0;
                    let mut decoded_bytes = 0;
                    for (column, &finish) in self.ends[..self.ends_len].iter().enumerate() {
                        let bytes = &self.output[start..finish];
                        ensure!(bytes.len() <= self.spec.max_field_bytes, "逻辑记录 {}，列 {}：字段超限", self.logical_records + 1, column + 1);
                        let decoded = match self.spec.encoding.as_str() {
                            "utf-8" => std::str::from_utf8(bytes).ok().map(std::borrow::Cow::Borrowed),
                            "gbk" => encoding_rs::GBK.decode_without_bom_handling_and_without_replacement(bytes),
                            _ => encoding_rs::GB18030.decode_without_bom_handling_and_without_replacement(bytes),
                        }.with_context(|| format!("逻辑记录 {}，字节 {}，物理行 {}，列 {}：非法 {} 编码",
                            self.logical_records + 1, self.record_start, self.record_line, column + 1, self.spec.encoding))?;
                        decoded_bytes += decoded.len();
                        ensure!(decoded.len() <= self.spec.max_field_bytes && decoded_bytes <= self.spec.max_record_bytes, "解码后记录或字段超过预算");
                        field(column, &decoded);
                        start = finish;
                    }
                    let record = CsvRecord { fields: vec![], field_count: self.ends_len, byte_start: self.record_start, line_start: self.record_line };
                    self.logical_records += 1;
                    self.record_start = self.consumed;
                    self.record_line = self.line;
                    self.output_len = 0;
                    self.ends_len = 0;
                    return Ok(ReadOutcome::Record(record));
                }
                ReadRecordResult::End => return Ok(ReadOutcome::End),
            }
        }
    }
}

pub fn validate_width(record: &CsvRecord, width: usize, rid: u64) -> Result<()> {
    if record.field_count != width {
        bail!("数据记录 {rid}，字节 {}，物理行 {}：期望 {width} 列，实际 {} 列",
            record.byte_start, record.line_start, record.field_count);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn records_survive_preview_budget_and_preserve_text() -> Result<()> {
        let mut file = tempfile::NamedTempFile::new()?;
        write!(file, "\u{feff}名称,编号,内容\r\n小区1,001,\"a,\r\n\"\"b\"\"\"\r\n小区2,9223372036854775807,\r\n")?;
        let path = file.into_temp_path();
        let cancel = AtomicBool::new(false);
        let (mut stream, _) = CsvStream::open(&path, ParseSpec::default())?;
        let mut records = vec![];
        loop {
            match stream.next(Some(7), &cancel)? {
                ReadOutcome::Record(r) => records.push(r.fields),
                ReadOutcome::Yield => continue,
                ReadOutcome::End => break,
            }
        }
        assert_eq!(records[1], ["小区1", "001", "a,\r\n\"b\""]);
        assert_eq!(records[2][1], "9223372036854775807");
        assert_eq!(records[2][2], "");
        assert_eq!(stream.hash(), blake3::hash(&std::fs::read(&path)?).to_hex().to_string());
        Ok(())
    }
    #[test]
    fn excessive_record_stops_before_unbounded_growth() -> Result<()> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(&vec![b'a'; 100_000])?;
        let path = file.into_temp_path();
        let spec = ParseSpec { max_record_bytes: 1024, ..Default::default() };
        let (mut stream, _) = CsvStream::open(&path, spec)?;
        assert!(stream.next(None, &AtomicBool::new(false)).is_err());
        Ok(())
    }
    #[test]
    fn borrowed_fields_match_owned_records_for_utf8_and_gb18030() -> Result<()> {
        let text = "名称,编号,内容\r\n中文,000123,\"a,\r\n\"\"b\"\"\"\r\n四字节𠀀,42,\r\n";
        for encoding in ["utf-8", "gb18030"] {
            let bytes = if encoding == "utf-8" { text.as_bytes().to_vec() }
                else { encoding_rs::GB18030.encode(text).0.into_owned() };
            let mut file = tempfile::NamedTempFile::new()?;
            file.write_all(&bytes)?;
            let path = file.into_temp_path();
            let spec = ParseSpec { encoding: encoding.into(), ..Default::default() };
            let (mut owned, _) = CsvStream::open(&path, spec.clone())?;
            let (mut borrowed, _) = CsvStream::open(&path, spec)?;
            let cancel = AtomicBool::new(false);
            loop {
                let mut fields = vec![];
                let actual = borrowed.next_into(Some(7), &cancel, |column, value| {
                    assert_eq!(column, fields.len());
                    fields.push(value.to_string());
                })?;
                match actual {
                    ReadOutcome::Yield => assert!(fields.is_empty()),
                    ReadOutcome::Record(record) => {
                        let ReadOutcome::Record(expected) = owned.next(None, &cancel)? else { panic!("missing record"); };
                        assert_eq!(fields, expected.fields);
                        assert_eq!(record.field_count, expected.fields.len());
                        assert_eq!((record.byte_start, record.line_start), (expected.byte_start, expected.line_start));
                    }
                    ReadOutcome::End => break,
                }
            }
            assert_eq!(owned.hash(), borrowed.hash());
        }
        Ok(())
    }
}
