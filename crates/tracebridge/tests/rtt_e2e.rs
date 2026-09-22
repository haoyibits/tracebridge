//! `tracebridge rtt` end to end against a fake RCL server that simulates a
//! SEGGER RTT control block in target memory.

#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::{FakeRcl, Project, State, command_in};

const CB: u64 = 0x2000_0000;
const UP: u64 = 0x2000_1000;
const DOWN: u64 = 0x2000_2000;

fn setup(state: &mut State) {
    state
        .symbols
        .insert("\\\\demo\\Global\\_SEGGER_RTT".into(), CB);
    state.write_memory(CB, b"SEGGER RTT\0\0\0\0\0\0");
    let words = |state: &mut State, at: u64, values: [u32; 4]| {
        for (i, value) in values.into_iter().enumerate() {
            state.write_memory(at + 4 + 4 * i as u64, &value.to_le_bytes());
        }
    };
    words(state, CB + 0x18, [UP as u32, 64, 6, 0]);
    words(state, CB + 0x30, [DOWN as u32, 16, 0, 0]);
    state.write_memory(UP, b"hello\n");
}

fn word(state: &State, address: u64) -> u32 {
    u32::from_le_bytes(state.read_memory(address, 4).try_into().unwrap())
}

fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn rtt_terminal_moves_data_both_ways_and_stops_on_ctrl_c() {
    let rcl = FakeRcl::start();
    setup(&mut rcl.state());
    let project = Project::new(rcl.port, "");
    let mut child = command_in(&project.root(), &["rtt", "--poll", "0.01"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdout = child.stdout.take().unwrap();
    let mut received = Vec::new();
    let reader = std::thread::spawn(move || {
        let mut buffer = [0u8; 256];
        while let Ok(count) = stdout.read(&mut buffer) {
            if count == 0 {
                break;
            }
            received.extend_from_slice(&buffer[..count]);
        }
        received
    });

    // Up-channel: the target's output is drained and RdOff advanced.
    wait_for(Duration::from_secs(10), || {
        word(&rcl.state(), CB + 0x18 + 0x10) == 6
    });

    // Down-channel: DEL is sent as BS.
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"ls\x7f\n")
        .unwrap();
    wait_for(Duration::from_secs(10), || {
        word(&rcl.state(), CB + 0x30 + 0x0C) == 4
    });
    assert_eq!(rcl.state().read_memory(DOWN, 4), b"ls\x08\n");

    // More output from the target.
    {
        let mut state = rcl.state();
        state.write_memory(UP + 6, b"again\n");
        state.write_memory(CB + 0x18 + 0x0C, &12u32.to_le_bytes());
    }
    wait_for(Duration::from_secs(10), || {
        word(&rcl.state(), CB + 0x18 + 0x10) == 12
    });

    let status = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let status = child.wait().unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(reader.join().unwrap(), b"hello\nagain\n");
    assert!(
        stderr.contains("TRACE32 RTT: _SEGGER_RTT @ 0x20000000; Ctrl-C to stop"),
        "{stderr}"
    );
    assert!(stderr.contains("TRACE32 RTT terminal stopped"), "{stderr}");
    let pid = child.id();
    assert!(
        rcl.state()
            .commands()
            .contains(&format!("ECHO \"RTT terminal connected (pid {pid})\""))
    );
}

#[test]
fn unknown_symbol_explains_what_to_do() {
    let rcl = FakeRcl::start();
    let project = Project::new(rcl.port, "");
    let output = project.run(&["rtt"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("tracebridge: cannot resolve _SEGGER_RTT in 'demo' ("),
        "{stderr}"
    );
    assert!(stderr.contains("Run 'tracebridge load' first, or pass --cb 0x<address>."));
}

#[test]
fn rtt_requires_powerview() {
    let project = Project::new(common::free_port(), "");
    let output = project.run(&["rtt", "--cb", "0x20000000"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no PowerView on RCL port"));
}

#[test]
fn rtt_help_lists_its_options() {
    let project = Project::new(common::free_port(), "");
    let output = project.run(&["rtt", "--help"]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    for option in [
        "--program",
        "--symbol",
        "--cb",
        "--node",
        "--port",
        "--poll",
        "--replay",
        "--output-only",
    ] {
        assert!(text.contains(option), "{option} missing:\n{text}");
    }
}

#[test]
fn waiting_message_when_rtt_is_not_initialized() {
    let rcl = FakeRcl::start();
    let project = Project::new(rcl.port, "");
    let mut child = command_in(
        &project.root(),
        &["rtt", "--cb", "0x20000000", "--output-only"],
    )
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    wait_for(Duration::from_secs(10), || {
        rcl.state()
            .log
            .iter()
            .filter(|l| l.starts_with("read 0x20000000 16"))
            .count()
            >= 3
    });
    std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    let status = child.wait().unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert!(
        stderr.contains("[rtt] waiting for the target to initialize SEGGER RTT"),
        "{stderr}"
    );
}

#[test]
fn terminal_mode_is_restored_after_ctrl_c() {
    let python = std::process::Command::new("python3")
        .arg("--version")
        .output();
    if !python.is_ok_and(|output| output.status.success()) {
        eprintln!("skipped: python3 not available");
        return;
    }
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/pty_rtt_check.py"
        ))
        .arg(env!("CARGO_BIN_EXE_tracebridge"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("while running: ICANON False ECHO False ISIG True"),
        "{text}"
    );
    assert!(text.contains("exit code 0"), "{text}");
    assert!(
        text.contains("after ^C:      ICANON True ECHO True"),
        "{text}"
    );
}
