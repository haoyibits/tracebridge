//! Replay recorded RCL sessions and require byte-identical client traffic.
//!
//! Every directory in tests/fixtures holds `client.bin` (bytes the Python
//! library sent), `server.bin` (bytes the server answered) and `session.txt`
//! (the operations, see tools/capture_session.py). The test repeats the
//! operations with `t32rcl` against a server that plays back `server.bin` and
//! checks that each request equals the recorded one.
//!
//! `python-fake` was recorded against tools/fake_t32.py. Recordings of a real
//! PowerView (made with the capture_proxy example) go next to it.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread;
use std::time::Duration;

use t32rcl::{Address, Debugger};

fn split_frames(mut bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    while bytes.len() >= 8 {
        let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        let total = (8 + length).div_ceil(8) * 8;
        let total = total.min(bytes.len());
        frames.push(bytes[..total].to_vec());
        bytes = &bytes[total..];
    }
    assert!(bytes.is_empty(), "trailing bytes in recording");
    frames
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn replay(dir: &Path) {
    let requests = split_frames(&fs::read(dir.join("client.bin")).unwrap());
    let answers = split_frames(&fs::read(dir.join("server.bin")).unwrap());
    let session = fs::read_to_string(dir.join("session.txt")).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || -> Result<usize, String> {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        for (index, expected) in requests.iter().enumerate() {
            let mut received = vec![0; expected.len()];
            stream
                .read_exact(&mut received)
                .map_err(|e| format!("request {index}: {e}"))?;
            if &received != expected {
                return Err(format!(
                    "request {index} differs\n expected {expected:02x?}\n received {received:02x?}"
                ));
            }
            if let Some(answer) = answers.get(index) {
                stream.write_all(answer).unwrap();
            }
        }
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
        if !rest.is_empty() {
            return Err(format!("{} unexpected trailing bytes", rest.len()));
        }
        Ok(requests.len())
    });

    let mut debugger: Option<Debugger> = None;
    for line in session.lines().filter(|line| !line.trim().is_empty()) {
        let (op, rest) = line.split_once(' ').unwrap_or((line, ""));
        if op == "connect" {
            debugger = Some(Debugger::connect("127.0.0.1", port, Duration::from_secs(5)).unwrap());
            continue;
        }
        let dbg = debugger.as_mut().expect("session must start with connect");
        let words: Vec<&str> = rest.split(' ').collect();
        match op {
            "print" => dbg.print(rest).unwrap(),
            "cmd" => dbg.cmd(rest).unwrap(),
            "system_up" => assert_eq!(dbg.system_up().unwrap(), words[0] == "True"),
            "state_run" => assert_eq!(dbg.state_run().unwrap(), words[0] == "True"),
            "fnc" => {
                dbg.fnc(words[0]).unwrap();
            }
            "read" => {
                let address = Address::parse(words[0]).unwrap();
                let data = dbg
                    .memory_read(&address, words[1].parse().unwrap())
                    .unwrap();
                assert_eq!(data, unhex(words[2]));
            }
            "write" => {
                let address = Address::parse(words[0]).unwrap();
                dbg.memory_write(&address, &unhex(words[1])).unwrap();
            }
            "write_u32" => {
                let address = Address::parse(words[0]).unwrap();
                dbg.memory_write_u32(&address, words[1].parse().unwrap())
                    .unwrap();
            }
            "symbol" => {
                assert_eq!(
                    dbg.symbol_address(words[0]).unwrap(),
                    words[1].parse::<u64>().unwrap()
                );
            }
            other => panic!("unknown session operation {other}"),
        }
    }
    drop(debugger);
    let count = server
        .join()
        .unwrap()
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    assert!(count > 0);
}

#[test]
fn recorded_sessions_are_reproduced_byte_for_byte() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut replayed = 0;
    for entry in fs::read_dir(&root).unwrap() {
        let dir = entry.unwrap().path();
        if dir.join("session.txt").is_file() {
            replay(&dir);
            replayed += 1;
        }
    }
    assert!(replayed > 0, "no fixtures in {}", root.display());
}
