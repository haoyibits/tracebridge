//! A generic fake TRACE32 RCL server for end-to-end tests of the binary.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

/// Simulated target state and a log of what the client did.
#[derive(Default)]
pub struct State {
    /// `cmd ...`, `fnc ...`, `read <addr> <len>`, `write <addr> <hex>`, `symbol <name>`.
    pub log: Vec<String>,
    pub system_up: bool,
    pub state_run: bool,
    pub memory: HashMap<u64, u8>,
    pub symbols: HashMap<String, u64>,
    /// Commands (by prefix) that fail with T32_ERR_FN1.
    pub failing: Vec<String>,
}

impl State {
    pub fn commands(&self) -> Vec<String> {
        self.log
            .iter()
            .filter_map(|line| line.strip_prefix("cmd ").map(str::to_string))
            .collect()
    }

    pub fn write_memory(&mut self, address: u64, data: &[u8]) {
        for (i, byte) in data.iter().enumerate() {
            self.memory.insert(address + i as u64, *byte);
        }
    }

    pub fn read_memory(&self, address: u64, length: usize) -> Vec<u8> {
        (0..length)
            .map(|i| *self.memory.get(&(address + i as u64)).unwrap_or(&0))
            .collect()
    }
}

pub struct FakeRcl {
    pub port: u16,
    pub state: Arc<Mutex<State>>,
}

impl FakeRcl {
    pub fn start() -> FakeRcl {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let state = shared.clone();
                thread::spawn(move || serve(stream, state));
            }
        });
        FakeRcl { port, state }
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut data = (payload.len() as u32).to_le_bytes().to_vec();
    data.extend_from_slice(&0x11u32.to_le_bytes());
    data.extend_from_slice(payload);
    data.resize(data.len().div_ceil(8) * 8, 0);
    data
}

fn function_answer(id: u8, result_type: u32, value: &str) -> Vec<u8> {
    let mut answer = vec![0, id];
    answer.extend_from_slice(&result_type.to_le_bytes());
    answer.extend_from_slice(&(value.len() as u32).to_le_bytes());
    answer.extend_from_slice(value.as_bytes());
    answer
}

fn text(payload: &[u8]) -> String {
    let body = &payload[4..];
    let end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    String::from_utf8_lossy(&body[..end]).into_owned()
}

/// Parse `u16 chunk | address parameters` and return (address, length, rest).
fn memory_request(payload: &[u8]) -> (u64, usize, usize) {
    let length = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    let address = u64::from_le_bytes(payload[4..12].try_into().unwrap());
    let mut index = 12;
    loop {
        match &payload[index..index + 2] {
            b"AC" => {
                let len = u16::from_le_bytes([payload[index + 2], payload[index + 3]]) as usize;
                index += 4 + len;
            }
            b"WI" => index += 4,
            b"XX" => return (address, length, index + 2),
            other => panic!("unexpected address parameter {other:?}"),
        }
    }
}

fn answer(state: &Mutex<State>, data: &[u8]) -> Vec<u8> {
    let (rapi_cmd, opt_arg, id) = (data[1], data[2], data[3]);
    let payload = if data[0] == 0 { &data[6..] } else { &data[4..] };
    let mut state = state.lock().unwrap();
    match (rapi_cmd, opt_arg) {
        (0x71, _) => vec![0, id],
        (0x72, 0x04) => {
            let command = text(payload);
            state.log.push(format!("cmd {command}"));
            if state
                .failing
                .iter()
                .any(|prefix| command.starts_with(prefix.as_str()))
            {
                let message = "command failed";
                let mut answer = vec![90, id, 0, 0, 0, 0];
                answer.extend_from_slice(&(message.len() as u32).to_le_bytes());
                answer.extend_from_slice(message.as_bytes());
                return answer;
            }
            match command.as_str() {
                "SYStem.Mode Attach" | "SYStem.Mode Up" | "SYStem.Up" => state.system_up = true,
                "SYStem.Mode Down" | "SYStem.Down" => state.system_up = false,
                "Go" => state.state_run = true,
                "Break" => state.state_run = false,
                _ => {}
            }
            vec![0, id]
        }
        (0x72, 0x05) => {
            let expression = text(payload);
            if !matches!(
                expression.as_str(),
                "SOFTWARE.BUILD()" | "SOFTWARE.BUILD.BASE()" | "VERSION.PYRCL(1.1.5)"
            ) {
                state.log.push(format!("fnc {expression}"));
            }
            let bool_text = |value: bool| if value { "TRUE()" } else { "FALSE()" };
            match expression.as_str() {
                "SOFTWARE.BUILD()" => function_answer(id, 0x0008, "190766."),
                "SOFTWARE.BUILD.BASE()" => function_answer(id, 0x0008, "187884."),
                "VERSION.PYRCL(1.1.5)" => function_answer(id, 0x0040, "OK"),
                "SYStem.Up()" => function_answer(id, 0x0001, bool_text(state.system_up)),
                "STATE.RUN()" => function_answer(id, 0x0001, bool_text(state.state_run)),
                "PRACTICE.SD()" => function_answer(id, 0x0008, "0."),
                other => panic!("unexpected function {other}"),
            }
        }
        (0x74, 0x35) => {
            let (address, length, _) = memory_request(payload);
            state.log.push(format!("read {address:#x} {length}"));
            let mut answer = vec![0, id];
            answer.extend(state.read_memory(address, length));
            answer
        }
        (0x74, 0x36) => {
            let (address, length, start) = memory_request(payload);
            let bytes = payload[start..start + length].to_vec();
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            state.log.push(format!("write {address:#x} {hex}"));
            state.write_memory(address, &bytes);
            vec![0, id]
        }
        (0x74, 0x68) => {
            let length = u16::from_le_bytes([payload[4], payload[5]]) as usize;
            let name = String::from_utf8_lossy(&payload[6..6 + length])
                .trim_end_matches('\0')
                .to_string();
            state.log.push(format!("symbol {name}"));
            let mut answer = vec![0, id];
            if let Some(address) = state.symbols.get(&name) {
                answer.extend_from_slice(b"AD\x03\x00");
                answer.extend_from_slice(&address.to_le_bytes());
                answer.extend_from_slice(b"AC\x02\x00D\x00XX");
            }
            answer.extend_from_slice(b"XX");
            answer
        }
        other => panic!("unexpected request {other:?}"),
    }
}

fn serve(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let count = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(count) => count,
        };
        buffer.extend_from_slice(&chunk[..count]);
        while buffer.len() >= 8 {
            let length = u32::from_le_bytes(buffer[..4].try_into().unwrap()) as usize;
            let total = (8 + length).div_ceil(8) * 8;
            if buffer.len() < total {
                break;
            }
            let reply = frame(&answer(&state, &buffer[8..8 + length]));
            buffer.drain(..total);
            if stream.write_all(&reply).is_err() {
                return;
            }
        }
    }
}

/// A project directory with a trace32.toml pointing at `rcl_port`.
pub struct Project {
    pub dir: tempfile::TempDir,
}

impl Project {
    pub fn new(rcl_port: u16, extra: &str) -> Project {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join("build/demo.elf"), b"\x7fELF").unwrap();
        std::fs::write(root.join("flash.cmm"), "").unwrap();
        std::fs::write(
            root.join("trace32.toml"),
            format!(
                "[project]\nprogram = \"demo\"\nelf = \"build/demo.elf\"\n\n\
                 [target]\ncpu = \"CORTEXM4\"\n\n\
                 [flash]\nscript = \"flash.cmm\"\n\n\
                 [trace32]\nsys = \"t32\"\nrcl_port = {rcl_port}\n{extra}"
            ),
        )
        .unwrap();
        Project { dir }
    }

    pub fn root(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path()).unwrap()
    }

    pub fn run(&self, args: &[&str]) -> Output {
        run_in(&self.root(), args)
    }
}

/// Run the binary with a clean environment so that a developer's T32SYS,
/// T32_BIN, ... can never make a test start the real PowerView.
pub fn command_in(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tracebridge"));
    command
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir);
    command
}

pub fn run_in(dir: &Path, args: &[&str]) -> Output {
    command_in(dir, args).output().unwrap()
}

pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
