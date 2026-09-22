//! A scripted fake TRACE32 RCL server for integration tests.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// One step of a scripted connection.
pub enum Step {
    /// Read exactly this request frame, then send `reply` (raw bytes, already framed).
    Exchange { request: Vec<u8>, reply: Vec<u8> },
    /// Read exactly this request frame and send nothing.
    Swallow(Vec<u8>),
    /// Send the reply one byte at a time.
    Trickle { request: Vec<u8>, reply: Vec<u8> },
    /// Close the connection.
    Close,
}

pub struct FakeServer {
    pub port: u16,
    handle: JoinHandle<Result<(), String>>,
}

impl FakeServer {
    /// Serve one scripted connection per entry of `connections`.
    pub fn start(connections: Vec<Vec<Step>>) -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            for (index, steps) in connections.into_iter().enumerate() {
                let (stream, _) = listener.accept().map_err(|e| e.to_string())?;
                serve(stream, steps).map_err(|e| format!("connection {index}: {e}"))?;
            }
            Ok(())
        });
        FakeServer { port, handle }
    }

    /// Wait for the server thread and fail the test on a script mismatch.
    pub fn finish(self) {
        self.handle.join().unwrap().unwrap();
    }
}

fn serve(mut stream: TcpStream, steps: Vec<Step>) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    for (index, step) in steps.into_iter().enumerate() {
        let expect = |stream: &mut TcpStream, request: &[u8]| -> Result<(), String> {
            let mut received = vec![0u8; request.len()];
            stream
                .read_exact(&mut received)
                .map_err(|e| format!("step {index}: reading request: {e}"))?;
            if received != request {
                return Err(format!(
                    "step {index}: request mismatch\n expected {}\n received {}",
                    hex(request),
                    hex(&received)
                ));
            }
            Ok(())
        };
        match step {
            Step::Exchange { request, reply } => {
                expect(&mut stream, &request)?;
                stream.write_all(&reply).map_err(|e| e.to_string())?;
            }
            Step::Trickle { request, reply } => {
                expect(&mut stream, &request)?;
                for byte in reply {
                    stream.write_all(&[byte]).map_err(|e| e.to_string())?;
                    stream.flush().unwrap();
                    thread::sleep(Duration::from_micros(200));
                }
            }
            Step::Swallow(request) => expect(&mut stream, &request)?,
            Step::Close => return Ok(()),
        }
    }
    // Wait for the client to close so it never sees an unexpected EOF.
    let mut rest = Vec::new();
    let _ = stream.read_to_end(&mut rest);
    if !rest.is_empty() {
        return Err(format!("unexpected trailing request {}", hex(&rest)));
    }
    Ok(())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn pad8(mut frame: Vec<u8>) -> Vec<u8> {
    let padding = (8 - frame.len() % 8) % 8;
    frame.resize(frame.len() + padding, 0);
    frame
}

/// A request frame as the Python library sends it.
pub fn request(rapi_cmd: u8, opt_arg: u8, id: u8, payload: &[u8], force: Option<usize>) -> Vec<u8> {
    let mut msg_len = force.unwrap_or(2 + payload.len());
    let mut data = Vec::new();
    if msg_len > 0xFF {
        msg_len += 2;
        data.extend_from_slice(&[0, rapi_cmd, opt_arg, id]);
        data.extend_from_slice(&(msg_len as u16).to_le_bytes());
    } else {
        data.extend_from_slice(&[msg_len as u8, rapi_cmd, opt_arg, id]);
    }
    data.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        data.push(0);
    }
    let mut frame = (data.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&0x10u32.to_le_bytes());
    frame.extend_from_slice(&data);
    pad8(frame)
}

/// A response frame (type 0x11) carrying `payload`.
pub fn frame(payload: &[u8], message_type: u32) -> Vec<u8> {
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&message_type.to_le_bytes());
    frame.extend_from_slice(payload);
    pad8(frame)
}

pub fn ok(id: u8, data: &[u8]) -> Vec<u8> {
    let mut payload = vec![0, id];
    payload.extend_from_slice(data);
    frame(&payload, 0x11)
}

pub fn error(id: u8, code: u8, message: &str) -> Vec<u8> {
    let mut payload = vec![code, id, 0, 0, 0, 0];
    payload.extend_from_slice(&(message.len() as u32).to_le_bytes());
    payload.extend_from_slice(message.as_bytes());
    frame(&payload, 0x11)
}

pub fn practice(text: &str) -> Vec<u8> {
    let mut payload = 4096u32.to_le_bytes().to_vec();
    payload.extend_from_slice(text.as_bytes());
    payload.push(0);
    payload
}

pub fn cmd_request(id: u8, command: &str) -> Vec<u8> {
    request(0x72, 0x04, id, &practice(command), None)
}

pub fn fnc_request(id: u8, expression: &str) -> Vec<u8> {
    request(0x72, 0x05, id, &practice(expression), None)
}

pub fn fnc_answer(id: u8, result_type: u32, value: &str) -> Vec<u8> {
    let mut data = result_type.to_le_bytes().to_vec();
    data.extend_from_slice(&(value.len() as u32).to_le_bytes());
    data.extend_from_slice(value.as_bytes());
    ok(id, &data)
}

pub fn cmd(id: u8, command: &str) -> Step {
    Step::Exchange {
        request: cmd_request(id, command),
        reply: ok(id, &[]),
    }
}

pub fn fnc(id: u8, expression: &str, result_type: u32, value: &str) -> Step {
    Step::Exchange {
        request: fnc_request(id, expression),
        reply: fnc_answer(id, result_type, value),
    }
}

/// Address parameters for `E:<value>` (A64, access class E).
pub fn e_address(value: u64, width: Option<u16>) -> Vec<u8> {
    let mut result = 3u16.to_le_bytes().to_vec();
    result.extend_from_slice(&value.to_le_bytes());
    result.extend_from_slice(b"AC\x02\x00E\x00");
    if let Some(width) = width {
        result.extend_from_slice(b"WI");
        result.extend_from_slice(&width.to_le_bytes());
    }
    result.extend_from_slice(b"XX");
    result
}

pub fn read_request(id: u8, address: u64, chunk: u16) -> Vec<u8> {
    let parameters = e_address(address, None);
    let mut payload = chunk.to_le_bytes().to_vec();
    payload.extend_from_slice(&parameters);
    request(0x74, 0x35, id, &payload, Some(parameters.len() + 6))
}

pub fn write_request(id: u8, address: u64, data: &[u8], width: Option<u16>) -> Vec<u8> {
    let parameters = e_address(address, width);
    let mut payload = (data.len() as u16).to_le_bytes().to_vec();
    payload.extend_from_slice(&parameters);
    payload.extend_from_slice(data);
    request(0x74, 0x36, id, &payload, Some(parameters.len() + 6))
}

/// The four exchanges of `rcl.connect`: attach and the version check.
pub fn handshake() -> Vec<Step> {
    vec![
        Step::Exchange {
            request: request(0x71, 0x01, 1, &[], None),
            reply: ok(1, &[]),
        },
        fnc(2, "SOFTWARE.BUILD()", 0x0008, "190766."),
        fnc(3, "SOFTWARE.BUILD.BASE()", 0x0008, "187884."),
        fnc(4, "VERSION.PYRCL(1.1.5)", 0x0040, "OK"),
    ]
}

/// The handshake followed by `steps`; request ids continue at 5.
pub fn session(steps: Vec<Step>) -> Vec<Step> {
    let mut all = handshake();
    all.extend(steps);
    all
}
