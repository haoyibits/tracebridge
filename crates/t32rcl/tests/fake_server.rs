//! Every public `Debugger` operation against a scripted fake RCL server.

mod common;

use std::time::Duration;

use common::*;
use t32rcl::{Address, Debugger, Error, Operation, Value};

const TIMEOUT: Duration = Duration::from_secs(2);

fn connect(server: &FakeServer) -> Debugger {
    Debugger::connect("localhost", server.port, TIMEOUT).unwrap()
}

#[test]
fn connect_attaches_and_checks_version() {
    let server = FakeServer::start(vec![handshake()]);
    let debugger = connect(&server);
    debugger.disconnect();
    server.finish();
}

#[test]
fn connect_rejects_old_powerview() {
    let server = FakeServer::start(vec![vec![
        Step::Exchange {
            request: request(0x71, 0x01, 1, &[], None),
            reply: ok(1, &[]),
        },
        fnc(2, "SOFTWARE.BUILD()", 0x0008, "100000."),
        fnc(3, "SOFTWARE.BUILD.BASE()", 0x0008, "100000."),
    ]]);
    let error = Debugger::connect("localhost", server.port, TIMEOUT).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Minimum required software version: 126615:125398, current version 100000:100000 (build:base)"
    );
    server.finish();
}

#[test]
fn attach_timeout_reconnects_once() {
    let server = FakeServer::start(vec![
        vec![Step::Swallow(request(0x71, 0x01, 1, &[], None))],
        // The message counter is not reset by the reconnection.
        vec![
            Step::Exchange {
                request: request(0x71, 0x01, 2, &[], None),
                reply: ok(2, &[]),
            },
            fnc(3, "SOFTWARE.BUILD()", 0x0008, "190766."),
            fnc(4, "SOFTWARE.BUILD.BASE()", 0x0008, "187884."),
            fnc(5, "VERSION.PYRCL(1.1.5)", 0x0040, "OK"),
        ],
    ]);
    let debugger = Debugger::connect("127.0.0.1", server.port, Duration::from_millis(300)).unwrap();
    drop(debugger);
    server.finish();
}

#[test]
fn connection_refused_is_a_connect_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let error = Debugger::connect("localhost", port, TIMEOUT).unwrap_err();
    assert!(matches!(error, Error::Connect(_)), "{error:?}");
}

#[test]
fn cmd_and_print() {
    let server = FakeServer::start(vec![session(vec![
        cmd(5, "SYStem.Mode Up"),
        cmd(6, "ECHO \"hello\""),
    ])]);
    let mut debugger = connect(&server);
    debugger.cmd("SYStem.Mode Up").unwrap();
    debugger.print("hello").unwrap();
    drop(debugger);
    server.finish();
}

#[test]
fn cmd_failure_names_the_command() {
    let server = FakeServer::start(vec![session(vec![Step::Exchange {
        request: cmd_request(5, "Bogus"),
        reply: error(5, 90, "unknown command"),
    }])]);
    let mut debugger = connect(&server);
    let failure = debugger.cmd("Bogus").unwrap_err();
    assert_eq!(failure.to_string(), "unknown command (command: Bogus)");
    match failure {
        Error::Trace32(error) => {
            assert_eq!(error.code, Some(90));
            assert_eq!(error.operation, Operation::Command("Bogus".into()));
        }
        other => panic!("{other:?}"),
    }
    drop(debugger);
    server.finish();
}

#[test]
fn other_error_codes_are_not_command_errors() {
    let server = FakeServer::start(vec![session(vec![Step::Exchange {
        request: cmd_request(5, "Go"),
        reply: ok_error_without_message(5, 3),
    }])]);
    let mut debugger = connect(&server);
    let failure = debugger.cmd("Go").unwrap_err();
    assert_eq!(failure.to_string(), "target not running");
    drop(debugger);
    server.finish();
}

fn ok_error_without_message(id: u8, code: u8) -> Vec<u8> {
    frame(&[code, id], 0x11)
}

#[test]
fn functions_decode_booleans() {
    let server = FakeServer::start(vec![session(vec![
        fnc(5, "SYStem.Up()", 0x0001, "TRUE()"),
        fnc(6, "STATE.RUN()", 0x0001, "FALSE()"),
        fnc(7, "OS.PWD()", 0x0040, "/tmp"),
    ])]);
    let mut debugger = connect(&server);
    assert!(debugger.system_up().unwrap());
    assert!(!debugger.state_run().unwrap());
    assert_eq!(
        debugger.fnc("OS.PWD()").unwrap(),
        Value::Text("/tmp".into())
    );
    drop(debugger);
    server.finish();
}

#[test]
fn function_failure() {
    let server = FakeServer::start(vec![session(vec![Step::Exchange {
        request: fnc_request(5, "NOPE()"),
        reply: error(5, 90, "unknown function"),
    }])]);
    let mut debugger = connect(&server);
    let failure = debugger.fnc("NOPE()").unwrap_err();
    assert_eq!(failure.to_string(), "unknown function");
    drop(debugger);
    server.finish();
}

#[test]
fn cmm_waits_for_the_practice_stack() {
    let server = FakeServer::start(vec![session(vec![
        fnc(5, "PRACTICE.SD()", 0x0008, "0."),
        cmd(6, "DO \"/p/flash.cmm\" PREPAREONLY"),
        fnc(7, "PRACTICE.SD()", 0x0008, "1."),
        fnc(8, "PRACTICE.SD()", 0x0008, "0."),
    ])]);
    let mut debugger = connect(&server);
    debugger
        .cmm("\"/p/flash.cmm\" PREPAREONLY", Some(Duration::from_secs(5)))
        .unwrap();
    drop(debugger);
    server.finish();
}

#[test]
fn cmm_times_out() {
    let mut steps = vec![fnc(5, "PRACTICE.SD()", 0x0008, "0."), cmd(6, "DO x.cmm")];
    for id in 7..=250 {
        steps.push(fnc(id, "PRACTICE.SD()", 0x0008, "1."));
    }
    let server = FakeServer::start(vec![session(steps)]);
    let mut debugger = connect(&server);
    let failure = debugger
        .cmm("x.cmm", Some(Duration::from_millis(30)))
        .unwrap_err();
    assert!(matches!(failure, Error::ScriptTimeout(_)), "{failure:?}");
    drop(debugger);
    // The server script is longer than needed; do not check it.
    let _ = server;
}

#[test]
fn cmm_detects_stack_underflow() {
    let server = FakeServer::start(vec![session(vec![
        fnc(5, "PRACTICE.SD()", 0x0008, "2."),
        cmd(6, "DO x.cmm"),
        fnc(7, "PRACTICE.SD()", 0x0008, "1."),
    ])]);
    let mut debugger = connect(&server);
    let failure = debugger.cmm("x.cmm", None).unwrap_err();
    assert_eq!(failure.to_string(), "Practice stack depth error");
    drop(debugger);
    server.finish();
}

#[test]
fn cmm_command_error_becomes_practice_error() {
    let server = FakeServer::start(vec![session(vec![
        fnc(5, "PRACTICE.SD()", 0x0008, "0."),
        Step::Exchange {
            request: cmd_request(6, "DO missing.cmm"),
            reply: error(6, 90, "file not found"),
        },
    ])]);
    let mut debugger = connect(&server);
    match debugger.cmm("missing.cmm", None).unwrap_err() {
        Error::Trace32(error) => {
            assert_eq!(error.operation, Operation::Practice);
            assert_eq!(error.to_string(), "file not found");
        }
        other => panic!("{other:?}"),
    }
    drop(debugger);
    server.finish();
}

#[test]
fn memory_read_in_chunks_of_1008() {
    let data: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
    let server = FakeServer::start(vec![session(vec![
        Step::Exchange {
            request: read_request(5, 0x2000_0000, 16),
            reply: ok(5, &data[..16]),
        },
        Step::Exchange {
            request: read_request(6, 0x2000_0000, 1008),
            reply: ok(6, &data[..1008]),
        },
        Step::Exchange {
            request: read_request(7, 0x2000_0000 + 1008, 992),
            reply: ok(7, &data[1008..]),
        },
    ])]);
    let mut debugger = connect(&server);
    let address = Address::parse("E:0x20000000").unwrap();
    assert_eq!(debugger.memory_read(&address, 16).unwrap(), data[..16]);
    assert_eq!(debugger.memory_read(&address, 2000).unwrap(), data);
    drop(debugger);
    server.finish();
}

#[test]
fn memory_read_failure() {
    let server = FakeServer::start(vec![session(vec![Step::Exchange {
        request: read_request(5, 0x10, 4),
        reply: error(5, 16, "bus error"),
    }])]);
    let mut debugger = connect(&server);
    let failure = debugger
        .memory_read(&Address::new(Some("E"), 0x10), 4)
        .unwrap_err();
    assert_eq!(failure.to_string(), "memory read failed: bus error");
    drop(debugger);
    server.finish();
}

#[test]
fn memory_write_in_chunks_of_1024_and_u32() {
    let data: Vec<u8> = (0..1500u32).map(|i| (i * 7) as u8).collect();
    let server = FakeServer::start(vec![session(vec![
        Step::Exchange {
            request: write_request(5, 0x100, &data[..1024], None),
            reply: ok(5, &[]),
        },
        Step::Exchange {
            request: write_request(6, 0x100 + 1024, &data[1024..], None),
            reply: ok(6, &[]),
        },
        Step::Exchange {
            request: write_request(7, 0x200, &0xDEAD_BEEFu32.to_le_bytes(), Some(4)),
            reply: ok(7, &[]),
        },
    ])]);
    let mut debugger = connect(&server);
    debugger
        .memory_write(&Address::new(Some("E"), 0x100), &data)
        .unwrap();
    debugger
        .memory_write_u32(&Address::new(Some("E"), 0x200), 0xDEAD_BEEF)
        .unwrap();
    drop(debugger);
    server.finish();
}

#[test]
fn memory_write_failure_with_wrong_parameters() {
    let server = FakeServer::start(vec![session(vec![Step::Exchange {
        request: write_request(5, 0x0, b"A", None),
        reply: error(5, 90, ""),
    }])]);
    let mut debugger = connect(&server);
    let failure = debugger
        .memory_write(&Address::new(Some("E"), 0), b"A")
        .unwrap_err();
    assert_eq!(failure.to_string(), "memory write failed: wrong parameters");
    drop(debugger);
    server.finish();
}

fn symbol_request(id: u8, name: &str) -> Vec<u8> {
    let length = (name.len() + 2) & !1;
    let mut stream = b"NM".to_vec();
    stream.extend_from_slice(&(length as u16).to_le_bytes());
    stream.extend_from_slice(name.as_bytes());
    stream.resize(4 + length, 0);
    stream.extend_from_slice(b"XX");
    let mut payload = ((stream.len() + 6) as u16).to_le_bytes().to_vec();
    payload.extend_from_slice(&stream);
    request(0x74, 0x68, id, &payload, None)
}

#[test]
fn symbol_query_resolves_address() {
    let name = "\\\\demo\\Global\\_SEGGER_RTT";
    let mut answer = b"NM\x0c\x00_SEGGER_RTT\x00".to_vec();
    answer.extend_from_slice(b"AD\x03\x00");
    answer.extend_from_slice(&0x2000_0400u64.to_le_bytes());
    answer.extend_from_slice(b"AC\x02\x00D\x00XX");
    answer.extend_from_slice(b"SZ");
    answer.extend_from_slice(&168u64.to_le_bytes());
    answer.extend_from_slice(b"XX");
    let server = FakeServer::start(vec![session(vec![Step::Exchange {
        request: symbol_request(5, name),
        reply: ok(5, &answer),
    }])]);
    let mut debugger = connect(&server);
    assert_eq!(debugger.symbol_address(name).unwrap(), 0x2000_0400);
    drop(debugger);
    server.finish();
}

#[test]
fn symbol_without_address_is_an_error() {
    let server = FakeServer::start(vec![session(vec![Step::Exchange {
        request: symbol_request(5, "x"),
        reply: ok(5, b"XX"),
    }])]);
    let mut debugger = connect(&server);
    assert!(debugger.symbol_address("x").is_err());
    drop(debugger);
    server.finish();
}

#[test]
fn answers_split_into_single_bytes() {
    let server = FakeServer::start(vec![session(vec![Step::Trickle {
        request: fnc_request(5, "SYStem.Up()"),
        reply: fnc_answer(5, 0x0001, "TRUE()"),
    }])]);
    let mut debugger = connect(&server);
    assert!(debugger.system_up().unwrap());
    drop(debugger);
    server.finish();
}

#[test]
fn keepalive_stale_answers_and_notifications_are_skipped() {
    let mut reply = frame(&[0xFE, 5], 0x11);
    reply.extend(frame(&[0, 9, 9], 0x12));
    reply.extend(ok(4, &[]));
    reply.extend(ok(5, &[]));
    let server = FakeServer::start(vec![session(vec![Step::Exchange {
        request: cmd_request(5, "Go"),
        reply,
    }])]);
    let mut debugger = connect(&server);
    debugger.cmd("Go").unwrap();
    drop(debugger);
    server.finish();
}

#[test]
fn answer_from_the_future_breaks_the_connection() {
    let server = FakeServer::start(vec![session(vec![Step::Exchange {
        request: cmd_request(5, "Go"),
        reply: ok(6, &[]),
    }])]);
    let mut debugger = connect(&server);
    let failure = debugger.cmd("Go").unwrap_err();
    assert_eq!(
        failure.to_string(),
        "Messages out of sync, connection broken"
    );
    drop(debugger);
    server.finish();
}

#[test]
fn silent_server_times_out() {
    let server = FakeServer::start(vec![session(vec![Step::Swallow(cmd_request(5, "Go"))])]);
    let mut debugger = connect(&server);
    debugger.set_timeout(Duration::from_millis(100)).unwrap();
    let failure = debugger.cmd("Go").unwrap_err();
    assert!(matches!(failure, Error::Timeout), "{failure:?}");
    assert_eq!(failure.to_string(), "timed out");
    drop(debugger);
    server.finish();
}

#[test]
fn closed_connection_is_a_connect_error() {
    let server = FakeServer::start(vec![session(vec![
        Step::Swallow(cmd_request(5, "Go")),
        Step::Close,
    ])]);
    let mut debugger = connect(&server);
    let failure = debugger.cmd("Go").unwrap_err();
    assert_eq!(failure.to_string(), "Connection closed by peer");
    drop(debugger);
    server.finish();
}

#[test]
fn message_ids_wrap_on_a_long_session() {
    let mut steps = Vec::new();
    // Ids 5..=254, then 0, 1, 2.
    let ids: Vec<u8> = (5..=254u8).chain([0, 1, 2]).collect();
    for &id in &ids {
        steps.push(cmd(id, "Go"));
    }
    let server = FakeServer::start(vec![session(steps)]);
    let mut debugger = connect(&server);
    for _ in &ids {
        debugger.cmd("Go").unwrap();
    }
    drop(debugger);
    server.finish();
}
