// Symbol objects ported from lauterbach-trace32-rcl 1.1.5 (MIT, Copyright (c) 2020
// Lauterbach GmbH): _rc/_symbol.py.

use crate::address::Address;
use crate::error::{Error, Result};

/// The answer of a symbol query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Symbol {
    pub name: Option<String>,
    pub path: Option<String>,
    pub address: Option<Address>,
    pub size: u64,
}

/// `Symbol.serialize` for a query by name.
///
/// Python computes the field length from the number of characters and then
/// packs the UTF-8 bytes into that many bytes (`struct.pack("{n}s")` pads with
/// NUL or truncates). Both agree for ASCII names, which is what TRACE32 uses.
pub(crate) fn serialize_name_query(name: &str) -> Vec<u8> {
    let length = (name.chars().count() + 2) & !1;
    let mut field = name.as_bytes().to_vec();
    field.resize(length, 0);
    let mut result = b"NM".to_vec();
    result.extend_from_slice(&(length as u16).to_le_bytes());
    result.extend_from_slice(&field);
    result.extend_from_slice(b"XX");
    result
}

/// `Symbol.deserialize`.
pub(crate) fn deserialize(buffer: &[u8]) -> Result<Symbol> {
    let truncated = || Error::Protocol("truncated symbol in answer".into());
    let read_u16 = |at: usize| -> Result<usize> {
        buffer
            .get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
            .ok_or_else(truncated)
    };
    let read_text = |at: usize, length: usize| -> Result<String> {
        let raw = buffer.get(at..at + length).ok_or_else(truncated)?;
        let text = std::str::from_utf8(raw)
            .map_err(|_| Error::Protocol("symbol text is not UTF-8".into()))?;
        Ok(text.trim_end_matches('\0').to_string())
    };

    let mut symbol = Symbol::default();
    let mut read_ptr = 0;
    while read_ptr < buffer.len() {
        let id = buffer.get(read_ptr..read_ptr + 2).ok_or_else(truncated)?;
        read_ptr += 2;
        let next = match id {
            b"AD" => {
                let (consumed, address) = Address::deserialize(&buffer[read_ptr..])?;
                symbol.address = Some(address);
                read_ptr + consumed
            }
            b"NM" | b"PT" | b"NE" => {
                let length = read_u16(read_ptr)?;
                read_ptr += 2;
                let text = read_text(read_ptr, length)?;
                match id {
                    b"NM" => symbol.name = Some(text),
                    b"PT" => symbol.path = Some(text),
                    _ => match &mut symbol.name {
                        Some(name) => name.push_str(&text),
                        None => {
                            return Err(Error::Protocol("name extension without a name".into()));
                        }
                    },
                }
                read_ptr + length
            }
            b"SZ" => {
                let raw = buffer.get(read_ptr..read_ptr + 8).ok_or_else(truncated)?;
                symbol.size = u64::from_le_bytes(raw.try_into().unwrap());
                read_ptr + 8
            }
            b"XX" => break,
            other => {
                return Err(Error::Protocol(format!(
                    "unknown symbol parameter {:?}",
                    String::from_utf8_lossy(other)
                )));
            }
        };
        read_ptr = next;
    }
    Ok(symbol)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_query_is_padded_to_even_length() {
        assert_eq!(serialize_name_query("ab"), b"NM\x04\x00ab\0\0XX");
        assert_eq!(serialize_name_query("abc"), b"NM\x04\x00abc\0XX");
    }

    #[test]
    fn deserializes_full_answer() {
        let mut buffer = b"NM\x06\x00_SEGG\0".to_vec();
        buffer.extend_from_slice(b"NE\x04\x00ER\0\0");
        buffer.extend_from_slice(b"PT\x04\x00\\\\a\0");
        buffer.extend_from_slice(b"AD\x03\x00");
        buffer.extend_from_slice(&0x2000_0400u64.to_le_bytes());
        buffer.extend_from_slice(b"AC\x02\x00D\0XX");
        buffer.extend_from_slice(b"SZ");
        buffer.extend_from_slice(&168u64.to_le_bytes());
        buffer.extend_from_slice(b"XX");
        let symbol = deserialize(&buffer).unwrap();
        assert_eq!(symbol.name.as_deref(), Some("_SEGGER"));
        assert_eq!(symbol.path.as_deref(), Some("\\\\a"));
        assert_eq!(symbol.address, Some(Address::new(Some("D"), 0x2000_0400)));
        assert_eq!(symbol.size, 168);
    }

    #[test]
    fn rejects_unknown_parameter() {
        assert!(deserialize(b"QQ\x00\x00").is_err());
    }
}
