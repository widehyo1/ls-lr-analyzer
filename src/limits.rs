//! Fixed input bounds, checked before copying or allocating record data.
use crate::Result;
use std::io::BufRead;

pub const FIND_FIELD: usize = 64 * 1024;
pub const FIND_RECORD: usize = 256 * 1024;
pub const TEXT_LINE: usize = 256 * 1024;
pub const HEX_LINE: usize = 2 * 1024 * 1024;
pub const SORT_CELL: usize = 1024 * 1024;
pub const SORT_RECORD: usize = 2 * 1024 * 1024;

pub fn delimited(
    reader: &mut impl BufRead,
    delimiter: u8,
    limit: usize,
    label: &str,
) -> Result<Option<(Vec<u8>, bool)>> {
    let mut value = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok((!value.is_empty()).then_some((value, false)));
        }
        let end = buffer.iter().position(|&b| b == delimiter);
        let length = end.unwrap_or(buffer.len());
        if length > limit.saturating_sub(value.len()) {
            return Err(format!("{label} exceeds {limit}-byte limit").into());
        }
        value.extend_from_slice(&buffer[..length]);
        reader.consume(length + usize::from(end.is_some()));
        if end.is_some() {
            return Ok(Some((value, true)));
        }
    }
}

pub fn line(reader: &mut impl BufRead) -> Result<Option<String>> {
    line_with_limit(reader, TEXT_LINE)
}

pub fn line_with_limit(reader: &mut impl BufRead, limit: usize) -> Result<Option<String>> {
    let Some((mut bytes, terminated)) = delimited(reader, b'\n', limit, "Input line")? else {
        return Ok(None);
    };
    if terminated && bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    Ok(Some(String::from_utf8(bytes)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};
    #[test]
    fn bounds_hold_across_buffer_boundaries() {
        let mut reader = BufReader::with_capacity(2, Cursor::new(b"abc\0defg\0"));
        assert_eq!(
            delimited(&mut reader, 0, 3, "Field").unwrap(),
            Some((b"abc".to_vec(), true))
        );
        assert!(delimited(&mut reader, 0, 3, "Field").is_err());
        assert_eq!(
            delimited(&mut Cursor::new(b"abc"), 0, 3, "Field").unwrap(),
            Some((b"abc".to_vec(), false))
        );
    }
}
