// Request encoding ported from lauterbach-trace32-rcl 1.1.5 (MIT, Copyright (c) 2020
// Lauterbach GmbH): `Library.generic_api_call` and the `t32_*` methods of
// _rc/_library.py that this client uses.

use crate::address::Address;
use crate::error::{Error, Result, Trace32Error};

pub(crate) const RAPI_CMD_ATTACH: u8 = 0x71;
pub(crate) const RAPI_CMD_EXECUTE_PRACTICE: u8 = 0x72;
pub(crate) const RAPI_CMD_DEVICE_SPECIFIC: u8 = 0x74;

/// `t32_executecommand`: EXECUTE_PRACTICE sub-command.
pub(crate) const EXECUTE_COMMAND: u8 = 0x04;
/// `t32_executefunction`: EXECUTE_PRACTICE sub-command.
pub(crate) const EXECUTE_FUNCTION: u8 = 0x05;
pub(crate) const RAPI_DSCMD_MEMORY_OBJ_READ: u8 = 0x35;
pub(crate) const RAPI_DSCMD_MEMORY_OBJ_WRITE: u8 = 0x36;
pub(crate) const RAPI_DSCMD_SYMBOL_QUERYOBJ: u8 = 0x68;

/// Result buffer size the Python library announces for commands and functions.
pub(crate) const RESULT_BUFFER_SIZE: u32 = 4096;
/// `Library._maxpacketsize`: the `packlen` passed to `connect` (always 1024 here).
pub(crate) const MAX_PACKET_SIZE: usize = 1024;
/// `CommunicationTcp.getHeadersize`.
pub(crate) const TCP_HEADER_SIZE: usize = 8 + 2;
/// Chunk size of `t32_readmemoryobj`: `down_align(packlen - headersize, 8)`.
pub(crate) const READ_CHUNK_SIZE: usize = (MAX_PACKET_SIZE - TCP_HEADER_SIZE) / 8 * 8;
/// Chunk size of `t32_writememoryobj`: `packlen - 0`.
pub(crate) const WRITE_CHUNK_SIZE: usize = MAX_PACKET_SIZE;

/// Encode one API request (`generic_api_call` without `force_16bit_length`).
pub(crate) fn encode_request(
    rapi_cmd: u8,
    opt_arg: u8,
    message_id: u8,
    payload: &[u8],
    force_length: Option<usize>,
) -> Result<Vec<u8>> {
    let mut msg_len = force_length.unwrap_or(2 + payload.len());
    let mut data = Vec::with_capacity(6 + payload.len() + 1);
    if msg_len > 0xFF {
        msg_len += 2;
        if msg_len > 0xF000 {
            return Err(Error::Protocol("message buffer too large".into()));
        }
        data.extend_from_slice(&[0, rapi_cmd, opt_arg, message_id]);
        data.extend_from_slice(&(msg_len as u16).to_le_bytes());
    } else {
        data.extend_from_slice(&[msg_len as u8, rapi_cmd, opt_arg, message_id]);
    }
    data.extend_from_slice(payload);
    data.resize(data.len() + payload.len() % 2, 0);
    Ok(data)
}

/// Check the status byte of an answer and return `answer[2..]` on success.
pub(crate) fn check_response(answer: &[u8]) -> std::result::Result<&[u8], Trace32Error> {
    let status = answer[0];
    if status == 0 {
        return Ok(&answer[2..]);
    }
    let message = if answer.len() > 10 {
        let length = u32::from_le_bytes(answer[6..10].try_into().unwrap()) as usize;
        let end = answer.len().min(10usize.saturating_add(length));
        std::str::from_utf8(&answer[10..end])
            .ok()
            .map(|text| text.trim_end_matches('\0').to_string())
    } else {
        None
    };
    Err(Trace32Error::from_code(status, message))
}

/// Payload of `t32_executecommand` and `t32_executefunction`.
pub(crate) fn practice_payload(text: &str) -> Vec<u8> {
    let mut payload = RESULT_BUFFER_SIZE.to_le_bytes().to_vec();
    payload.extend_from_slice(text.as_bytes());
    payload.push(0);
    payload
}

/// Payload and forced length of one `t32_readmemoryobj` chunk.
pub(crate) fn memory_read_payload(
    address: &Address,
    offset: u64,
    chunk: usize,
) -> (Vec<u8>, usize) {
    let parameters = address.serialize(offset, None);
    let mut payload = (chunk as u16).to_le_bytes().to_vec();
    payload.extend_from_slice(&parameters);
    (payload, parameters.len() + 6)
}

/// Payload and forced length of one `t32_writememoryobj` chunk.
pub(crate) fn memory_write_payload(
    address: &Address,
    offset: u64,
    data: &[u8],
    width: Option<u16>,
) -> (Vec<u8>, usize) {
    let parameters = address.serialize(offset, width);
    let mut payload = (data.len() as u16).to_le_bytes().to_vec();
    payload.extend_from_slice(&parameters);
    payload.extend_from_slice(data);
    (payload, parameters.len() + 6)
}

/// Payload of `t32_querysymbolobj`. The Python library adds 6 to the stream
/// length to work around a TRACE32 quirk ("illegal character for this context").
pub(crate) fn symbol_query_payload(stream: &[u8]) -> Vec<u8> {
    let mut payload = ((stream.len() + 6) as u16).to_le_bytes().to_vec();
    payload.extend_from_slice(stream);
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        text.split_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }

    fn frame(rapi_cmd: u8, opt_arg: u8, id: u8, payload: &[u8], force: Option<usize>) -> Vec<u8> {
        crate::link::encode_frame(&encode_request(rapi_cmd, opt_arg, id, payload, force).unwrap())
    }

    /// Frames produced by the Python library itself (crates/t32rcl/tools/reference_bytes.py).
    #[test]
    fn frames_match_python_reference_bytes() {
        let address = Address::new(Some("E"), 0x2000_0000);
        assert_eq!(
            frame(RAPI_CMD_ATTACH, 1, 1, &[], None),
            hex("04 00 00 00 10 00 00 00 02 71 01 01 00 00 00 00")
        );
        assert_eq!(
            frame(
                RAPI_CMD_EXECUTE_PRACTICE,
                EXECUTE_COMMAND,
                2,
                &practice_payload("Go"),
                None
            ),
            hex("0c 00 00 00 10 00 00 00 09 72 04 02 00 10 00 00 47 6f 00 00 00 00 00 00")
        );
        assert_eq!(
            frame(
                RAPI_CMD_EXECUTE_PRACTICE,
                EXECUTE_FUNCTION,
                3,
                &practice_payload("SYStem.Up()"),
                None
            ),
            hex(
                "14 00 00 00 10 00 00 00 12 72 05 03 00 10 00 00 53 59 53 74 65 6d 2e 55 70 28 29 00 00 00 00 00"
            )
        );
        let (payload, force) = memory_read_payload(&address, 0, 16);
        assert_eq!(
            frame(
                RAPI_CMD_DEVICE_SPECIFIC,
                RAPI_DSCMD_MEMORY_OBJ_READ,
                4,
                &payload,
                Some(force)
            ),
            hex(
                "18 00 00 00 10 00 00 00 18 74 35 04 10 00 03 00 00 00 00 20 00 00 00 00 41 43 02 00 45 00 58 58"
            )
        );
        let (payload, force) = memory_write_payload(&address, 0, b"AB", None);
        assert_eq!(
            frame(
                RAPI_CMD_DEVICE_SPECIFIC,
                RAPI_DSCMD_MEMORY_OBJ_WRITE,
                5,
                &payload,
                Some(force)
            ),
            hex(
                "1a 00 00 00 10 00 00 00 18 74 36 05 02 00 03 00 00 00 00 20 00 00 00 00 41 43 02 00 45 00 58 58 41 42 00 00 00 00 00 00"
            )
        );
        let (payload, force) = memory_write_payload(&address, 0, &7u32.to_le_bytes(), Some(4));
        assert_eq!(
            frame(
                RAPI_CMD_DEVICE_SPECIFIC,
                RAPI_DSCMD_MEMORY_OBJ_WRITE,
                6,
                &payload,
                Some(force)
            ),
            hex(
                "20 00 00 00 10 00 00 00 1c 74 36 06 04 00 03 00 00 00 00 20 00 00 00 00 41 43 02 00 45 00 57 49 04 00 58 58 07 00 00 00"
            )
        );
        let stream = crate::symbol::serialize_name_query("\\\\app\\Global\\_SEGGER_RTT");
        assert_eq!(
            frame(
                RAPI_CMD_DEVICE_SPECIFIC,
                RAPI_DSCMD_SYMBOL_QUERYOBJ,
                7,
                &symbol_query_payload(&stream),
                None
            ),
            hex(
                "26 00 00 00 10 00 00 00 24 74 68 07 26 00 4e 4d 1a 00 5c 5c 61 70 70 5c 47 6c 6f 62 61 6c 5c 5f 53 45 47 47 45 52 5f 52 54 54 00 00 58 58 00 00"
            )
        );
    }

    #[test]
    fn chunk_sizes_follow_packlen_1024() {
        assert_eq!(READ_CHUNK_SIZE, 1008);
        assert_eq!(WRITE_CHUNK_SIZE, 1024);
    }

    #[test]
    fn short_request_is_padded_to_even_length() {
        let data = encode_request(0x72, 0x04, 2, &practice_payload("Go"), None).unwrap();
        assert_eq!(
            data,
            [
                0x09, 0x72, 0x04, 0x02, 0x00, 0x10, 0x00, 0x00, b'G', b'o', 0, 0
            ]
        );
    }

    #[test]
    fn long_request_carries_16_bit_length() {
        let command = "A".repeat(300);
        let payload = practice_payload(&command);
        let data = encode_request(0x72, 0x04, 9, &payload, None).unwrap();
        assert_eq!(&data[..4], [0, 0x72, 0x04, 9]);
        assert_eq!(
            u16::from_le_bytes([data[4], data[5]]) as usize,
            payload.len() + 4
        );
        assert_eq!(data.len(), 6 + payload.len() + payload.len() % 2);
    }

    #[test]
    fn oversized_request_is_rejected() {
        let payload = vec![0u8; 0xF000];
        let error = encode_request(0x72, 0x04, 1, &payload, None).unwrap_err();
        assert_eq!(error.to_string(), "message buffer too large");
    }

    #[test]
    fn forced_length_keeps_the_short_form() {
        let address = Address::new(Some("E"), 0x2000_0000);
        let (payload, length) = memory_write_payload(&address, 0, &[0x55; 1024], None);
        let data = encode_request(0x74, 0x36, 1, &payload, Some(length)).unwrap();
        assert_eq!(data[0], 24);
        assert_eq!(data.len(), 4 + payload.len());
    }

    #[test]
    fn error_answer_carries_message() {
        let mut answer = vec![90, 3, 0, 0, 0, 0];
        answer.extend_from_slice(&5u32.to_le_bytes());
        answer.extend_from_slice(b"oops\0");
        let error = check_response(&answer).unwrap_err();
        assert_eq!(error.code, Some(90));
        assert_eq!(error.message, "oops");
    }

    #[test]
    fn short_error_answer_uses_default_message() {
        let error = check_response(&[3, 1]).unwrap_err();
        assert_eq!(error.message, "target not running");
    }

    #[test]
    fn success_answer_strips_status_and_id() {
        assert_eq!(check_response(&[0, 4, 1, 2]).unwrap(), [1, 2]);
    }
}
