//! Join private DuckDB batches into one Parquet file without retaining its footer.
//! Compact Thrift and offset fields follow apache/parquet-format/parquet.thrift.
//! This is deliberately restricted to plaintext batches produced by our query.
use crate::Result;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAX_FOOTER: usize = 8 * 1024 * 1024;
const MAX_NODES: usize = 131_072;
pub(super) const BATCH_ROWS: usize = 8192;

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Bool(bool),
    Byte(u8),
    Int(u8, i64),
    Double([u8; 8]),
    Binary(Vec<u8>),
    List(u8, Vec<Value>),
    Struct(Vec<(i16, Value)>),
}

impl Value {
    fn kind(&self) -> u8 {
        match self {
            Self::Bool(true) => 1,
            Self::Bool(false) => 2,
            Self::Byte(_) => 3,
            Self::Int(kind, _) => *kind,
            Self::Double(_) => 7,
            Self::Binary(_) => 8,
            Self::List(_, _) => 9,
            Self::Struct(_) => 12,
        }
    }
    fn fields(&mut self) -> Result<&mut Vec<(i16, Value)>> {
        match self {
            Self::Struct(fields) => Ok(fields),
            _ => Err("Invalid Parquet metadata struct".into()),
        }
    }
    fn field(&self, id: i16) -> Result<&Value> {
        match self {
            Self::Struct(fields) => fields
                .iter()
                .find(|(key, _)| *key == id)
                .map(|(_, value)| value)
                .ok_or_else(|| format!("Missing Parquet metadata field {id}").into()),
            _ => Err("Invalid Parquet metadata struct".into()),
        }
    }
    fn integer(&self) -> Result<i64> {
        match self {
            Self::Int(_, n) => Ok(*n),
            _ => Err("Invalid Parquet metadata integer".into()),
        }
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    nodes: usize,
}
impl Decoder<'_> {
    fn byte(&mut self) -> Result<u8> {
        let (&first, rest) = self
            .bytes
            .split_first()
            .ok_or("Truncated Parquet metadata")?;
        self.bytes = rest;
        Ok(first)
    }
    fn varint(&mut self) -> Result<u64> {
        let mut n = 0;
        for shift in (0..70).step_by(7) {
            let byte = self.byte()?;
            if shift == 63 && byte > 1 {
                return Err("Parquet metadata varint overflow".into());
            }
            n |= u64::from(byte & 127) << shift;
            if byte & 128 == 0 {
                return Ok(n);
            }
        }
        Err("Invalid Parquet metadata varint".into())
    }
    fn integer(&mut self) -> Result<i64> {
        let n = self.varint()?;
        Ok(((n >> 1) as i64) ^ -((n & 1) as i64))
    }
    fn value(&mut self, kind: u8, depth: usize) -> Result<Value> {
        self.nodes += 1;
        if self.nodes > MAX_NODES || depth > 32 {
            return Err("Parquet batch metadata exceeds memory limit".into());
        }
        Ok(match kind {
            1 | 2 => Value::Bool(kind == 1),
            3 => Value::Byte(self.byte()?),
            4..=6 => Value::Int(kind, self.integer()?),
            7 => {
                let bytes = self
                    .bytes
                    .get(..8)
                    .ok_or("Truncated Parquet double")?
                    .try_into()?;
                self.bytes = &self.bytes[8..];
                Value::Double(bytes)
            }
            8 => {
                let length = usize::try_from(self.varint()?)?;
                let bytes = self
                    .bytes
                    .get(..length)
                    .ok_or("Truncated Parquet binary")?
                    .to_vec();
                self.bytes = &self.bytes[length..];
                Value::Binary(bytes)
            }
            9 => {
                let header = self.byte()?;
                let kind = header & 15;
                let count = if header >> 4 == 15 {
                    usize::try_from(self.varint()?)?
                } else {
                    usize::from(header >> 4)
                };
                if count > self.bytes.len() || count > MAX_NODES - self.nodes {
                    return Err("Parquet metadata list exceeds memory limit".into());
                }
                let mut values = Vec::new();
                for _ in 0..count {
                    let item_kind = if kind == 1 { self.byte()? } else { kind };
                    values.push(self.value(item_kind, depth + 1)?);
                }
                Value::List(kind, values)
            }
            12 => {
                let mut fields = Vec::new();
                let mut previous = 0_i16;
                loop {
                    let header = self.byte()?;
                    if header == 0 {
                        break;
                    }
                    let id = if header >> 4 == 0 {
                        i16::try_from(self.integer()?)?
                    } else {
                        previous
                            .checked_add(i16::from(header >> 4))
                            .ok_or("Parquet field ID overflow")?
                    };
                    if id <= previous {
                        return Err("Unexpected Parquet metadata field order".into());
                    }
                    fields.push((id, self.value(header & 15, depth + 1)?));
                    previous = id;
                }
                Value::Struct(fields)
            }
            _ => return Err("Unsupported Parquet metadata type".into()),
        })
    }
}

fn varint(writer: &mut impl Write, mut n: u64) -> io::Result<()> {
    while n >= 128 {
        writer.write_all(&[(n as u8) | 128])?;
        n >>= 7;
    }
    writer.write_all(&[n as u8])
}
fn integer(writer: &mut impl Write, n: i64) -> io::Result<()> {
    varint(writer, ((n as u64) << 1) ^ ((n >> 63) as u64))
}
fn field_header(writer: &mut impl Write, previous: &mut i16, id: i16, kind: u8) -> io::Result<()> {
    let delta = i32::from(id) - i32::from(*previous);
    if (1..=15).contains(&delta) {
        writer.write_all(&[((delta as u8) << 4) | kind])?;
    } else {
        writer.write_all(&[kind])?;
        integer(writer, i64::from(id))?;
    }
    *previous = id;
    Ok(())
}
fn list_header(writer: &mut impl Write, count: usize, kind: u8) -> io::Result<()> {
    if count < 15 {
        writer.write_all(&[((count as u8) << 4) | kind])
    } else {
        writer.write_all(&[240 | kind])?;
        varint(writer, count as u64)
    }
}
fn encode(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    match value {
        Value::Bool(_) => Ok(()), // The boolean value is in the struct field header.
        Value::Byte(byte) => writer.write_all(&[*byte]),
        Value::Int(_, n) => integer(writer, *n),
        Value::Double(bytes) => writer.write_all(bytes),
        Value::Binary(bytes) => {
            varint(writer, bytes.len() as u64)?;
            writer.write_all(bytes)
        }
        Value::List(kind, values) => {
            list_header(writer, values.len(), *kind)?;
            for value in values {
                if *kind == 1 {
                    writer.write_all(&[value.kind()])?;
                }
                encode(writer, value)?;
            }
            Ok(())
        }
        Value::Struct(fields) => {
            let mut previous = 0;
            for (id, value) in fields {
                field_header(writer, &mut previous, *id, value.kind())?;
                encode(writer, value)?;
            }
            writer.write_all(&[0])
        }
    }
}

fn relocate(fields: &mut [(i16, Value)], ids: &[i16], delta: i64, end: u64) -> Result<()> {
    for (id, value) in fields {
        if ids.contains(id) {
            let Value::Int(6, offset) = value else {
                return Err("Invalid Parquet offset".into());
            };
            if *offset == 0 {
                continue;
            }
            if *offset < 4 || *offset as u64 >= end {
                return Err("Parquet offset outside batch data".into());
            }
            *offset = offset.checked_add(delta).ok_or("Parquet offset overflow")?;
        }
    }
    Ok(())
}

pub struct Joiner {
    output: BufWriter<File>,
    groups: BufWriter<File>,
    groups_path: std::path::PathBuf,
    template: Option<Value>,
    rows: i64,
    count: usize,
}
impl Joiner {
    pub fn new(output: &Path, groups_path: &Path) -> Result<Self> {
        let mut writer = BufWriter::new(File::create(output)?);
        writer.write_all(b"PAR1")?;
        Ok(Self {
            output: writer,
            groups: BufWriter::new(File::create(groups_path)?),
            groups_path: groups_path.into(),
            template: None,
            rows: 0,
            count: 0,
        })
    }
    pub fn append(&mut self, batch: &Path) -> Result<()> {
        let mut file = File::open(batch)?;
        let length = file.metadata()?.len();
        if length < 12 {
            return Err("Invalid Parquet batch length".into());
        }
        let mut magic = [0; 4];
        file.read_exact(&mut magic)?;
        if &magic != b"PAR1" {
            return Err("Invalid Parquet batch header".into());
        }
        file.seek(SeekFrom::End(-8))?;
        let mut tail = [0; 8];
        file.read_exact(&mut tail)?;
        if &tail[4..] != b"PAR1" {
            return Err("Encrypted or invalid Parquet batch footer".into());
        }
        let footer_len = u32::from_le_bytes(tail[..4].try_into()?) as usize;
        if footer_len > MAX_FOOTER || footer_len as u64 > length - 12 {
            return Err("Parquet batch footer exceeds memory limit".into());
        }
        let end = length - 8 - footer_len as u64;
        file.seek(SeekFrom::Start(end))?;
        let mut bytes = vec![0; footer_len];
        file.read_exact(&mut bytes)?;
        let mut decoder = Decoder {
            bytes: &bytes,
            nodes: 0,
        };
        let mut metadata = decoder.value(12, 0)?;
        if !decoder.bytes.is_empty() {
            return Err("Trailing Parquet metadata bytes".into());
        }
        if metadata.fields()?.iter().any(|(id, _)| *id >= 8) {
            return Err("Encrypted Parquet batches are not supported".into());
        }
        if let Some(template) = &self.template
            && template.field(2)? != metadata.field(2)?
        {
            return Err("Parquet batch schema mismatch".into());
        }
        let row_count = metadata.field(3)?.integer()?;
        if !(0..=BATCH_ROWS as i64).contains(&row_count) {
            return Err("Parquet batch row count exceeds limit".into());
        }
        self.rows = self
            .rows
            .checked_add(row_count)
            .ok_or("Parquet row count overflow")?;
        let position = self.output.stream_position()?;
        let delta = i64::try_from(position)?
            .checked_sub(4)
            .ok_or("Parquet offset overflow")?;
        file.seek(SeekFrom::Start(4))?;
        if io::copy(&mut file.take(end - 4), &mut self.output)? != end - 4 {
            return Err("Truncated Parquet batch data".into());
        }
        let fields = metadata.fields()?;
        let (_, groups) = fields
            .iter_mut()
            .find(|(id, _)| *id == 4)
            .ok_or("Missing Parquet row groups")?;
        let Value::List(12, groups) = groups else {
            return Err("Invalid Parquet row group list".into());
        };
        let mut group_rows = 0_i64;
        for mut group in groups.drain(..) {
            group_rows = group_rows
                .checked_add(group.field(3)?.integer()?)
                .ok_or("Parquet group row count overflow")?;
            let fields = group.fields()?;
            fields.retain(|(id, _)| *id != 7); // Batch ordinals do not apply to the joined file.
            relocate(fields, &[5], delta, end)?;
            let (_, columns) = fields
                .iter_mut()
                .find(|(id, _)| *id == 1)
                .ok_or("Missing Parquet columns")?;
            let Value::List(12, columns) = columns else {
                return Err("Invalid Parquet column list".into());
            };
            if columns.len() != 13 {
                return Err("Unexpected Parquet batch column count".into());
            }
            for column in columns {
                let fields = column.fields()?;
                if fields.iter().any(|(id, _)| matches!(id, 1 | 8 | 9)) {
                    return Err("External/encrypted Parquet chunks are not supported".into());
                }
                // Page offset indexes contain nested absolute offsets. Omit both
                // optional indexes rather than retaining stale references.
                fields.retain(|(id, _)| !matches!(id, 4..=7));
                relocate(fields, &[2], delta, end)?;
                let (_, meta) = fields
                    .iter_mut()
                    .find(|(id, _)| *id == 3)
                    .ok_or("Missing Parquet column metadata")?;
                relocate(meta.fields()?, &[9, 10, 11, 14], delta, end)?;
            }
            encode(&mut self.groups, &group)?;
            self.count = self
                .count
                .checked_add(1)
                .filter(|&n| n <= i32::MAX as usize)
                .ok_or("Parquet row group count overflow")?;
        }
        if group_rows != row_count {
            return Err("Parquet batch row count mismatch".into());
        }
        if self.groups.stream_position()? > u32::MAX as u64 {
            return Err("Parquet footer exceeds format size limit".into());
        }
        if self.template.is_none() {
            self.template = Some(metadata);
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        self.groups.flush()?;
        let start = self.output.stream_position()?;
        let mut previous = 0;
        let mut template = self.template.take().ok_or("Missing Parquet schema")?;
        for (id, value) in template.fields()? {
            field_header(&mut self.output, &mut previous, *id, value.kind())?;
            match *id {
                3 => integer(&mut self.output, self.rows)?,
                4 => {
                    list_header(&mut self.output, self.count, 12)?;
                    io::copy(&mut File::open(&self.groups_path)?, &mut self.output)?;
                }
                _ => encode(&mut self.output, value)?,
            }
        }
        self.output.write_all(&[0])?;
        let length = u32::try_from(self.output.stream_position()? - start)
            .map_err(|_| "Parquet footer exceeds format size limit")?;
        self.output.write_all(&length.to_le_bytes())?;
        self.output.write_all(b"PAR1")?;
        self.output.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_metadata_is_bounded() {
        for bytes in [
            vec![0x18, 0xff, 0xff, 0xff, 0xff, 0x7f],
            vec![0x19, 0xfc, 0xff, 0xff, 0x7f],
            vec![0x16, 0xff],
        ] {
            let mut decoder = Decoder {
                bytes: &bytes,
                nodes: 0,
            };
            assert!(decoder.value(12, 0).is_err());
        }
    }
}
