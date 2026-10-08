//! Bounded-memory external sorting of private, length-prefixed records.
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
use std::path::PathBuf;

use crate::Result;
use crate::export::temp::Staging;

const BUFFER_BYTES: usize = 8 * 1024 * 1024;
const FAN_IN: u64 = 16;

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Record {
    pub key: (String, String),
    pub data: Vec<u8>,
}

pub fn write_record(writer: &mut impl Write, record: &Record) -> io::Result<()> {
    for bytes in [
        record.key.0.as_bytes(),
        record.key.1.as_bytes(),
        &record.data,
    ] {
        writer.write_all(&(bytes.len() as u64).to_le_bytes())?;
        writer.write_all(bytes)?;
    }
    Ok(())
}

fn read_bytes(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0; 8];
    reader.read_exact(&mut length)?;
    let length = usize::try_from(u64::from_le_bytes(length))
        .map_err(|_| io::Error::other("Temporary record is too large"))?;
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub fn read_record(reader: &mut impl io::BufRead) -> Result<Option<Record>> {
    if reader.fill_buf()?.is_empty() {
        return Ok(None);
    }
    Ok(Some(Record {
        key: (
            String::from_utf8(read_bytes(reader)?)?,
            String::from_utf8(read_bytes(reader)?)?,
        ),
        data: read_bytes(reader)?,
    }))
}

pub struct Sorter {
    stage: Staging,
    buffer: Vec<Record>,
    bytes: usize,
    budget: usize,
    runs: u64,
}

impl Sorter {
    pub fn new() -> Result<Self> {
        Self::with_budget(BUFFER_BYTES)
    }

    fn with_budget(budget: usize) -> Result<Self> {
        Ok(Self {
            stage: Staging::new(&std::env::temp_dir())?,
            buffer: Vec::new(),
            bytes: 0,
            budget,
            runs: 0,
        })
    }

    pub fn push(&mut self, record: Record) -> Result<()> {
        // Charge two struct slots per record to cover Vec's geometric capacity.
        let bytes = 2 * size_of::<Record>()
            + record.key.0.capacity()
            + record.key.1.capacity()
            + record.data.capacity();
        if !self.buffer.is_empty() && self.bytes.saturating_add(bytes) > self.budget {
            self.flush()?;
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.buffer.push(record);
        if self.bytes >= self.budget {
            self.flush()?;
        }
        Ok(())
    }

    fn path(&self, pass: u64, run: u64) -> PathBuf {
        self.stage.0.join(format!("run-{pass}-{run}"))
    }

    fn flush(&mut self) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.buffer.sort_unstable();
        let mut writer = BufWriter::new(File::create(self.path(0, self.runs))?);
        for record in self.buffer.drain(..) {
            write_record(&mut writer, &record)?;
        }
        writer.flush()?;
        self.runs += 1;
        self.bytes = 0;
        Ok(())
    }

    pub fn finish(mut self) -> Result<Sorted> {
        self.flush()?;
        // Drop the buffer before allocating merge readers. Run paths are computed
        // from counters: no manifest growing with the number of input records.
        self.buffer = Vec::new();
        let mut pass = 0;
        if self.runs == 0 {
            File::create(self.path(0, 0))?;
        }
        while self.runs > 1 {
            let mut output_run = 0;
            for first in (0..self.runs).step_by(FAN_IN as usize) {
                let end = (first + FAN_IN).min(self.runs);
                let mut readers = (first..end)
                    .map(|run| File::open(self.path(pass, run)).map(BufReader::new))
                    .collect::<io::Result<Vec<_>>>()?;
                let mut heap = BinaryHeap::new();
                for (index, reader) in readers.iter_mut().enumerate() {
                    if let Some(record) = read_record(reader)? {
                        heap.push(Reverse((record, index)));
                    }
                }
                let mut writer = BufWriter::new(File::create(self.path(pass + 1, output_run))?);
                while let Some(Reverse((record, index))) = heap.pop() {
                    write_record(&mut writer, &record)?;
                    if let Some(record) = read_record(&mut readers[index])? {
                        heap.push(Reverse((record, index)));
                    }
                }
                writer.flush()?;
                drop(readers);
                for run in first..end {
                    fs::remove_file(self.path(pass, run))?;
                }
                output_run += 1;
            }
            self.runs = output_run;
            pass += 1;
        }
        let reader = BufReader::new(File::open(self.path(pass, 0))?);
        Ok(Sorted {
            reader,
            _stage: self.stage,
        })
    }
}

pub struct Sorted {
    reader: BufReader<File>,
    _stage: Staging,
}

impl Sorted {
    pub fn next(&mut self) -> Result<Option<Record>> {
        read_record(&mut self.reader)
    }

    pub fn rewind(&mut self) -> Result<()> {
        self.reader.rewind()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiple_merge_passes_preserve_binary_cells_and_duplicate_keys() {
        let mut sorter = Sorter::with_budget(256).unwrap();
        let mut expected = Vec::new();
        for i in (0..600).rev() {
            let record = Record {
                key: (format!("{}\0\n한글", i % 17), format!("{}", i % 3)),
                data: (i as u64).to_le_bytes().to_vec(),
            };
            sorter
                .push(Record {
                    key: record.key.clone(),
                    data: record.data.clone(),
                })
                .unwrap();
            expected.push(record);
        }
        expected.sort();
        let mut sorted = sorter.finish().unwrap();
        for record in expected {
            assert_eq!(sorted.next().unwrap(), Some(record));
        }
        assert!(sorted.next().unwrap().is_none());
        sorted.rewind().unwrap();
        assert!(sorted.next().unwrap().is_some());
    }
}
