// TCP transport ported from lauterbach-trace32-rcl 1.1.5 (MIT, Copyright (c) 2020
// Lauterbach GmbH): `CommunicationTcp`, `CommunicationBase.receive` and `Link` in
// _rc/hlinknet.py.
//
// One deliberate deviation: `CommunicationTcp.extract_message` discards a buffer
// that holds less than a complete 8-byte frame header, and skips alignment bytes
// that have not been received yet, which desynchronizes the stream when TCP splits
// a frame at those points. This port keeps partial headers buffered and skips the
// missing alignment bytes when they arrive.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::error::{Error, Result};

/// `T32_NETTCP_RCL_REQ`: frame type of a request.
pub(crate) const RCL_REQ: u32 = 0x0010;
/// `T32_NETTCP_RCL_RESP`: frame type of a response.
pub(crate) const RCL_RESP: u32 = 0x0011;
/// `T32_NETTCP_RCL_NOTIFY`: frame type of a notification.
pub(crate) const RCL_NOTIFY: u32 = 0x0012;
/// `T32_API_KEEPALIVE`: first byte of a response that must be ignored.
pub(crate) const KEEPALIVE: u8 = 0xFE;

const HEADER_LEN: usize = 8;
const ALIGN64: usize = 8;
/// `Link` always creates `CommunicationTcp(0x4000, ...)`; this is the size of one
/// socket read.
const RECEIVE_SIZE: usize = 0x4000;

/// `align_eight` in _rc/common.py: bytes needed to reach the next multiple of 8.
pub(crate) fn align_eight(n: usize) -> usize {
    (ALIGN64 - n % ALIGN64) % ALIGN64
}

/// Wrap message data in a NETTCP request frame (`CommunicationTcp.transmit`).
pub(crate) fn encode_frame(data: &[u8]) -> Vec<u8> {
    let size = data.len();
    let mut frame = Vec::with_capacity(HEADER_LEN + size + ALIGN64);
    frame.extend_from_slice(&(size as u32).to_le_bytes());
    frame.extend_from_slice(&RCL_REQ.to_le_bytes());
    frame.extend_from_slice(data);
    frame.resize(frame.len() + align_eight(size + HEADER_LEN), 0);
    frame
}

/// Incremental parser for frames received from TRACE32.
#[derive(Debug, Default)]
pub(crate) struct FrameDecoder {
    buffer: Vec<u8>,
    /// Alignment bytes of the previous frame that have not been received yet.
    pending_padding: usize,
}

impl FrameDecoder {
    /// Feed received bytes; complete responses are appended to `responses`.
    /// Notifications are dropped because this client never enables them.
    pub(crate) fn feed(
        &mut self,
        mut data: &[u8],
        responses: &mut VecDeque<Vec<u8>>,
    ) -> Result<()> {
        let skipped = self.pending_padding.min(data.len());
        self.pending_padding -= skipped;
        data = &data[skipped..];
        self.buffer.extend_from_slice(data);

        loop {
            if self.buffer.len() < HEADER_LEN {
                return Ok(());
            }
            let payload_len = u32::from_le_bytes(self.buffer[0..4].try_into().unwrap()) as usize;
            let message_len = payload_len + HEADER_LEN;
            if self.buffer.len() < message_len {
                return Ok(());
            }
            let message_type = u32::from_le_bytes(self.buffer[4..8].try_into().unwrap());
            let payload = self.buffer[HEADER_LEN..message_len].to_vec();
            // TRACE32 aligns every frame to 64 bits by appending unused bytes.
            let stuffed_len = message_len.div_ceil(ALIGN64) * ALIGN64;
            let consumed = stuffed_len.min(self.buffer.len());
            self.pending_padding = stuffed_len - consumed;
            self.buffer.drain(..consumed);

            match message_type {
                RCL_RESP => responses.push_back(payload),
                RCL_NOTIFY => {}
                _ => {
                    return Err(Error::Protocol(
                        "TCP packet contains invalid messagetype".into(),
                    ));
                }
            }
        }
    }
}

/// The message-id counter of `Link`.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct MessageId(u64);

impl MessageId {
    /// `Link.increment_message_id`: advance, then return the id for a new request.
    pub(crate) fn next(&mut self) -> u8 {
        self.0 += 1;
        self.current()
    }

    /// `Link.get_message_id`: the id of the answer that is expected next.
    pub(crate) fn current(&self) -> u8 {
        (self.0 % 255) as u8
    }
}

/// Pick the next queued response for `expected` (`CommunicationBase.receive`).
/// Returns `Ok(None)` when the queue holds no matching response yet.
pub(crate) fn take_response(
    responses: &mut VecDeque<Vec<u8>>,
    expected: u8,
) -> Result<Option<Vec<u8>>> {
    while let Some(message) = responses.pop_front() {
        if message.is_empty() || message[0] == KEEPALIVE {
            continue;
        }
        let Some(&id) = message.get(1) else {
            return Err(Error::Protocol("response is too short".into()));
        };
        if id < expected {
            continue;
        }
        if id == expected {
            return Ok(Some(message));
        }
        return Err(Error::Connect(
            "Messages out of sync, connection broken".into(),
        ));
    }
    Ok(None)
}

/// A NETTCP connection to TRACE32.
#[derive(Debug)]
pub(crate) struct Link {
    stream: TcpStream,
    decoder: FrameDecoder,
    responses: VecDeque<Vec<u8>>,
}

impl Link {
    /// Connect over IPv4 like `socket.socket(AF_INET, SOCK_STREAM)`; `timeout`
    /// applies to the connection attempt and to every later send and receive.
    pub(crate) fn connect(node: &str, port: u16, timeout: Duration) -> Result<Link> {
        let addresses: Vec<SocketAddr> = (node, port)
            .to_socket_addrs()
            .map_err(|error| Error::Connect(error.to_string()))?
            .filter(SocketAddr::is_ipv4)
            .collect();
        let Some(address) = addresses.first() else {
            return Err(Error::Connect(format!("no IPv4 address for {node}")));
        };
        let stream = TcpStream::connect_timeout(address, timeout).map_err(Error::from_io)?;
        stream.set_nodelay(true).map_err(Error::from_io)?;
        let link = Link {
            stream,
            decoder: FrameDecoder::default(),
            responses: VecDeque::new(),
        };
        link.set_timeout(timeout)?;
        Ok(link)
    }

    pub(crate) fn set_timeout(&self, timeout: Duration) -> Result<()> {
        let timeout = (!timeout.is_zero()).then_some(timeout);
        self.stream
            .set_read_timeout(timeout)
            .and_then(|()| self.stream.set_write_timeout(timeout))
            .map_err(Error::from_io)
    }

    pub(crate) fn transmit(&mut self, data: &[u8]) -> Result<()> {
        self.stream
            .write_all(&encode_frame(data))
            .map_err(Error::from_io)
    }

    pub(crate) fn receive(&mut self, expected: u8) -> Result<Vec<u8>> {
        loop {
            if let Some(message) = take_response(&mut self.responses, expected)? {
                return Ok(message);
            }
            self.poll_response()?;
        }
    }

    fn poll_response(&mut self) -> Result<()> {
        let mut chunk = vec![0u8; RECEIVE_SIZE];
        let count = loop {
            match self.stream.read(&mut chunk) {
                Ok(count) => break count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(Error::from_io(error)),
            }
        };
        if count == 0 {
            return Err(Error::Connect("Connection closed by peer".into()));
        }
        self.decoder.feed(&chunk[..count], &mut self.responses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_frame(payload: &[u8], message_type: u32) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&message_type.to_le_bytes());
        frame.extend_from_slice(payload);
        frame.resize(frame.len() + align_eight(payload.len() + 8), 0);
        frame
    }

    #[test]
    fn align_eight_matches_python() {
        assert_eq!(align_eight(8), 0);
        assert_eq!(align_eight(12), 4);
        assert_eq!(align_eight(15), 1);
        assert_eq!(align_eight(16), 0);
    }

    #[test]
    fn request_frame_is_padded_to_eight_bytes() {
        assert_eq!(
            encode_frame(&[0x02, 0x71, 0x01, 0x01]),
            [
                4, 0, 0, 0, 0x10, 0, 0, 0, 0x02, 0x71, 0x01, 0x01, 0, 0, 0, 0
            ]
        );
        assert_eq!(encode_frame(&[0; 8]).len(), 16);
    }

    #[test]
    fn decodes_frames_split_at_every_byte() {
        let mut stream = response_frame(&[0, 1, 0xAA], RCL_RESP);
        stream.extend(response_frame(&[0, 2], RCL_NOTIFY));
        stream.extend(response_frame(&[0, 3, 1, 2, 3, 4, 5, 6, 7, 8], RCL_RESP));
        let mut decoder = FrameDecoder::default();
        let mut responses = VecDeque::new();
        for byte in &stream {
            decoder
                .feed(std::slice::from_ref(byte), &mut responses)
                .unwrap();
        }
        assert_eq!(
            Vec::from(responses),
            vec![vec![0, 1, 0xAA], vec![0, 3, 1, 2, 3, 4, 5, 6, 7, 8]]
        );
    }

    #[test]
    fn decodes_several_frames_in_one_read() {
        let mut stream = response_frame(&[0, 1], RCL_RESP);
        stream.extend(response_frame(&[0, 2], RCL_RESP));
        let mut decoder = FrameDecoder::default();
        let mut responses = VecDeque::new();
        decoder.feed(&stream, &mut responses).unwrap();
        assert_eq!(responses.len(), 2);
    }

    #[test]
    fn rejects_unknown_frame_type() {
        let mut decoder = FrameDecoder::default();
        let error = decoder
            .feed(&response_frame(&[0, 1], 0x99), &mut VecDeque::new())
            .unwrap_err();
        assert_eq!(error.to_string(), "TCP packet contains invalid messagetype");
    }

    #[test]
    fn message_id_wraps_after_254() {
        let mut id = MessageId::default();
        assert_eq!(id.next(), 1);
        let mut id = MessageId(253);
        assert_eq!(id.next(), 254);
        assert_eq!(id.current(), 254);
        assert_eq!(id.next(), 0);
        assert_eq!(id.next(), 1);
    }

    #[test]
    fn response_matching_skips_keepalive_and_stale_answers() {
        let mut queue = VecDeque::from(vec![
            vec![],
            vec![KEEPALIVE, 7],
            vec![0, 6, 0xAA],
            vec![0, 7, 0xBB],
        ]);
        assert_eq!(
            take_response(&mut queue, 7).unwrap(),
            Some(vec![0, 7, 0xBB])
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn response_from_the_future_breaks_the_connection() {
        let mut queue = VecDeque::from(vec![vec![0, 9]]);
        let error = take_response(&mut queue, 7).unwrap_err();
        assert_eq!(error.to_string(), "Messages out of sync, connection broken");
    }

    #[test]
    fn empty_queue_needs_more_data() {
        assert_eq!(take_response(&mut VecDeque::new(), 1).unwrap(), None);
    }
}
