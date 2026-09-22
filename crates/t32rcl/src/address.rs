// Address objects ported from lauterbach-trace32-rcl 1.1.5 (MIT, Copyright (c) 2020
// Lauterbach GmbH): _rc/_address.py.

use std::fmt;

use crate::error::{Error, Result};

const T32_ADDRTYPE_A32: u16 = 2;
const T32_ADDRTYPE_A64: u16 = 3;

/// A TRACE32 address: an optional access class such as `E` and a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub access: Option<String>,
    pub value: u64,
}

impl Address {
    pub fn new(access: Option<&str>, value: u64) -> Self {
        Address {
            access: access.map(str::to_string),
            value,
        }
    }

    /// `Address.from_string`: parse `[access:]value` where value is decimal or
    /// `0x` hexadecimal. Parsing is local; nothing is sent to TRACE32.
    ///
    /// The Python regular expression also has machine-id (`:::`) and space-id
    /// (`::`) groups, but the greedy access group always wins, and an address
    /// with either group set is rejected by `Address.__init__`. The effective
    /// rule is therefore: everything before the last colon is the access class.
    pub fn parse(text: &str) -> Result<Address> {
        let invalid = || Error::Protocol(format!("invalid address: {text}"));
        if text.contains('\n') {
            return Err(invalid());
        }
        let (access, value) = match text.rfind(':') {
            // The access group needs at least one character.
            Some(0) => return Err(invalid()),
            Some(index) => (Some(&text[..index]), &text[index + 1..]),
            None => (None, text),
        };
        let value = if let Some(hex) = value.strip_prefix("0x") {
            if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid());
            }
            u64::from_str_radix(hex, 16).map_err(|_| invalid())?
        } else {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            // int(value, 0) rejects decimal literals with leading zeros.
            if value.len() > 1 && value.starts_with('0') && value.bytes().any(|b| b != b'0') {
                return Err(invalid());
            }
            value.parse().map_err(|_| invalid())?
        };
        Ok(Address {
            access: access.map(str::to_string),
            value,
        })
    }

    /// `Address.serialize`: always the 64-bit address type.
    pub(crate) fn serialize(&self, offset: u64, width: Option<u16>) -> Vec<u8> {
        let mut result = T32_ADDRTYPE_A64.to_le_bytes().to_vec();
        result.extend_from_slice(&self.value.wrapping_add(offset).to_le_bytes());
        if let Some(access) = &self.access {
            let bytes = access.as_bytes();
            let mut length = bytes.len() + 1;
            length += length % 2;
            result.extend_from_slice(b"AC");
            result.extend_from_slice(&(length as u16).to_le_bytes());
            result.extend_from_slice(bytes);
            result.resize(result.len() + length - bytes.len(), 0);
        }
        if let Some(width) = width {
            result.extend_from_slice(b"WI");
            result.extend_from_slice(&width.to_le_bytes());
        }
        result.extend_from_slice(b"XX");
        result
    }

    /// `Address.deserialize`: returns the number of bytes consumed and the address.
    pub(crate) fn deserialize(buffer: &[u8]) -> Result<(usize, Address)> {
        let truncated = || Error::Protocol("truncated address in answer".into());
        let read_u16 = |at: usize| -> Result<u16> {
            buffer
                .get(at..at + 2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .ok_or_else(truncated)
        };
        let address_type = read_u16(0)?;
        let (value, mut read_ptr) = match address_type {
            T32_ADDRTYPE_A32 => {
                let bytes = buffer.get(2..6).ok_or_else(truncated)?;
                (u32::from_le_bytes(bytes.try_into().unwrap()) as u64, 6)
            }
            T32_ADDRTYPE_A64 => {
                let bytes = buffer.get(2..10).ok_or_else(truncated)?;
                (u64::from_le_bytes(bytes.try_into().unwrap()), 10)
            }
            other => {
                return Err(Error::Protocol(format!("unsupported address type {other}")));
            }
        };

        let mut access = None;
        while read_ptr < buffer.len() {
            let id = buffer.get(read_ptr..read_ptr + 2).ok_or_else(truncated)?;
            read_ptr += 2;
            let next = match id {
                b"AC" => {
                    let length = read_u16(read_ptr)? as usize;
                    read_ptr += 2;
                    let raw = buffer
                        .get(read_ptr..read_ptr + length)
                        .ok_or_else(truncated)?;
                    access = Some(parse_access(raw)?);
                    read_ptr + length
                }
                b"WI" | b"CO" | b"MU" | b"TU" => read_ptr + 2,
                b"SI" | b"IM" | b"AT" => read_ptr + 4,
                b"XX" => break,
                other => {
                    return Err(Error::Protocol(format!(
                        "unknown address parameter {:?}",
                        String::from_utf8_lossy(other)
                    )));
                }
            };
            read_ptr = next;
        }
        Ok((read_ptr, Address { access, value }))
    }
}

/// `Address.parse_access`: strip NUL padding and the trailing colon.
fn parse_access(raw: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| Error::Protocol("access class is not UTF-8".into()))?;
    Ok(text.trim_matches('\0').trim_matches(':').to_string())
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.access {
            Some(access) => write!(f, "{access}:0x{:08x}", self.value),
            None => write!(f, "0x{:08x}", self.value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_access_and_hex_value() {
        assert_eq!(
            Address::parse("E:0x20000000").unwrap(),
            Address::new(Some("E"), 0x2000_0000)
        );
        assert_eq!(Address::parse("4096").unwrap(), Address::new(None, 4096));
        assert_eq!(
            Address::parse("A:B:0x10").unwrap(),
            Address::new(Some("A:B"), 0x10)
        );
    }

    #[test]
    fn rejects_values_python_rejects() {
        for text in ["E:", ":0x10", "E:0x", "E:0X10", "E:0123", "E:12a", ""] {
            assert!(Address::parse(text).is_err(), "{text}");
        }
        assert_eq!(Address::parse("E:00").unwrap().value, 0);
    }

    #[test]
    fn serializes_access_class_and_width() {
        let address = Address::new(Some("E"), 0x2000_0000);
        assert_eq!(
            address.serialize(0x10, Some(4)),
            [
                3, 0, 0x10, 0, 0, 0x20, 0, 0, 0, 0, b'A', b'C', 2, 0, b'E', 0, b'W', b'I', 4, 0,
                b'X', b'X'
            ]
        );
        // Two-letter access: length 3 is rounded up to 4.
        let address = Address::new(Some("AD"), 0);
        assert_eq!(&address.serialize(0, None)[10..18], b"AC\x04\x00AD\0\0");
    }

    #[test]
    fn deserializes_answer_address() {
        let mut buffer = vec![2, 0, 0x78, 0x56, 0x34, 0x12];
        buffer.extend_from_slice(b"AC\x04\x00SD:\0WI\x04\x00XX");
        let (consumed, address) = Address::deserialize(&buffer).unwrap();
        assert_eq!(consumed, buffer.len());
        assert_eq!(address, Address::new(Some("SD"), 0x1234_5678));
    }

    #[test]
    fn display_matches_python_str() {
        assert_eq!(Address::new(Some("E"), 0x10).to_string(), "E:0x00000010");
    }
}
