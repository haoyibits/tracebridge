//! Debug Adapter Protocol framing (dap/protocol.py).

use serde_json::{Map, Value};

pub const MAX_HEADER_BYTES: usize = 16 * 1024;
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

pub type Message = Map<String, Value>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DapProtocolError(pub String);

/// `encode_message`: compact JSON (`separators=(",", ":")`, non-ASCII kept)
/// behind a Content-Length header.
pub fn encode_message(message: &Message) -> Result<Vec<u8>, DapProtocolError> {
    let body = serde_json::to_vec(message)
        .map_err(|error| DapProtocolError(format!("cannot encode DAP message: {error}")))?;
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(DapProtocolError(format!(
            "DAP message exceeds {MAX_MESSAGE_BYTES} bytes"
        )));
    }
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// `DapDecoder`: accumulates bytes and yields complete messages.
#[derive(Debug)]
pub struct DapDecoder {
    buffer: Vec<u8>,
    max_header_bytes: usize,
    max_message_bytes: usize,
}

impl Default for DapDecoder {
    fn default() -> Self {
        DapDecoder::new(MAX_HEADER_BYTES, MAX_MESSAGE_BYTES)
    }
}

impl DapDecoder {
    pub fn new(max_header_bytes: usize, max_message_bytes: usize) -> Self {
        DapDecoder {
            buffer: Vec::new(),
            max_header_bytes,
            max_message_bytes,
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<Message>, DapProtocolError> {
        self.buffer.extend_from_slice(chunk);
        let mut messages = Vec::new();
        loop {
            let Some(separator) = find(&self.buffer, b"\r\n\r\n") else {
                if self.buffer.len() > self.max_header_bytes {
                    return Err(DapProtocolError("DAP header is too large".into()));
                }
                return Ok(messages);
            };
            if separator > self.max_header_bytes {
                return Err(DapProtocolError("DAP header is too large".into()));
            }
            // Python decodes the header as strict ASCII; a failure there is a
            // protocol error here.
            let header = std::str::from_utf8(&self.buffer[..separator])
                .ok()
                .filter(|text| text.is_ascii())
                .ok_or_else(|| DapProtocolError("DAP header is not ASCII".into()))?;
            let mut length = None;
            for line in header.split("\r\n") {
                if let Some((name, value)) = line.split_once(':') {
                    if name.trim().eq_ignore_ascii_case("content-length") {
                        let value = value.trim();
                        length = Some(crate::pycompat::parse_decimal(value).ok_or_else(|| {
                            DapProtocolError(format!("invalid DAP Content-Length: {value}"))
                        })?);
                        break;
                    }
                }
            }
            let Some(length) = length else {
                return Err(DapProtocolError("DAP message has no Content-Length".into()));
            };
            if length < 0 || length as u64 > self.max_message_bytes as u64 {
                return Err(DapProtocolError(format!(
                    "invalid DAP message length: {length}"
                )));
            }
            let message_end = separator + 4 + length as usize;
            if self.buffer.len() < message_end {
                return Ok(messages);
            }
            let payload: Vec<u8> = self
                .buffer
                .drain(..message_end)
                .skip(separator + 4)
                .collect();
            let decoded: Value = std::str::from_utf8(&payload)
                .map_err(|error| DapProtocolError(format!("invalid DAP JSON: {error}")))
                .and_then(|text| {
                    serde_json::from_str(text)
                        .map_err(|error| DapProtocolError(format!("invalid DAP JSON: {error}")))
                })?;
            match decoded {
                Value::Object(message) => messages.push(message),
                _ => return Err(DapProtocolError("DAP payload must be a JSON object".into())),
            }
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(value: Value) -> Message {
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    // test_dap_protocol.py: test_decodes_split_message
    #[test]
    fn decodes_split_message() {
        let request = message(json!({"seq": 1, "type": "request", "command": "initialize"}));
        let encoded = encode_message(&request).unwrap();
        let mut decoder = DapDecoder::default();
        let midpoint = encoded.len() / 2;
        assert!(decoder.feed(&encoded[..midpoint]).unwrap().is_empty());
        assert_eq!(decoder.feed(&encoded[midpoint..]).unwrap(), [request]);
    }

    // test_dap_protocol.py: test_decodes_multiple_messages
    #[test]
    fn decodes_multiple_messages() {
        let first = message(json!({"seq": 1}));
        let second = message(json!({"seq": 2}));
        let mut stream = encode_message(&first).unwrap();
        stream.extend(encode_message(&second).unwrap());
        assert_eq!(
            DapDecoder::default().feed(&stream).unwrap(),
            [first, second]
        );
    }

    // test_dap_protocol.py: test_rejects_missing_content_length
    #[test]
    fn rejects_missing_content_length() {
        let error = DapDecoder::default()
            .feed(b"Other: value\r\n\r\n{}")
            .unwrap_err();
        assert_eq!(error.0, "DAP message has no Content-Length");
    }

    // test_dap_protocol.py: test_rejects_oversized_message
    #[test]
    fn rejects_oversized_message() {
        let error = DapDecoder::new(MAX_HEADER_BYTES, 4)
            .feed(b"Content-Length: 5\r\n\r\n12345")
            .unwrap_err();
        assert_eq!(error.0, "invalid DAP message length: 5");
    }

    #[test]
    fn encodes_compact_json_with_raw_unicode() {
        let encoded = encode_message(&message(json!({"a": "ü", "b": [1, 2]}))).unwrap();
        assert_eq!(
            encoded,
            "Content-Length: 20\r\n\r\n{\"a\":\"ü\",\"b\":[1,2]}".as_bytes()
        );
    }

    #[test]
    fn keeps_key_order_and_numbers() {
        let text = r#"{"z":1,"a":12345678901234567890123,"m":1.50}"#;
        let frame = format!("Content-Length: {}\r\n\r\n{text}", text.len());
        let decoded = DapDecoder::default().feed(frame.as_bytes()).unwrap();
        assert_eq!(serde_json::to_string(&decoded[0]).unwrap(), text);
    }

    #[test]
    fn header_is_case_insensitive_and_first_length_wins() {
        let frame = b"X: 1\r\ncontent-LENGTH:  2 \r\nContent-Length: 99\r\n\r\n{}";
        assert_eq!(DapDecoder::default().feed(frame).unwrap().len(), 1);
    }

    #[test]
    fn rejects_bad_headers_and_payloads() {
        let cases: [(&[u8], &str); 5] = [
            (
                b"Content-Length: x\r\n\r\n",
                "invalid DAP Content-Length: x",
            ),
            (
                b"Content-Length: -1\r\n\r\n",
                "invalid DAP message length: -1",
            ),
            (
                b"Content-Length: 2\r\n\r\n[]",
                "DAP payload must be a JSON object",
            ),
            (b"Content-Length: 1\r\n\r\n{", "invalid DAP JSON: "),
            (
                b"Content-L\xc3\xa4ngth: 1\r\n\r\n{",
                "DAP header is not ASCII",
            ),
        ];
        for (input, expected) in cases {
            let error = DapDecoder::default().feed(input).unwrap_err();
            assert!(error.0.starts_with(expected), "{input:?}: {error}");
        }
    }

    #[test]
    fn rejects_endless_header() {
        let mut decoder = DapDecoder::new(8, MAX_MESSAGE_BYTES);
        assert!(decoder.feed(b"Content-").unwrap().is_empty());
        assert_eq!(decoder.feed(b"L").unwrap_err().0, "DAP header is too large");
    }

    #[test]
    fn byte_by_byte_feeding() {
        let request = message(json!({"seq": 7, "command": "next"}));
        let mut stream = encode_message(&request).unwrap();
        stream.extend(encode_message(&request).unwrap());
        let mut decoder = DapDecoder::default();
        let mut decoded = Vec::new();
        for byte in stream {
            decoded.extend(decoder.feed(&[byte]).unwrap());
        }
        assert_eq!(decoded.len(), 2);
    }
}
