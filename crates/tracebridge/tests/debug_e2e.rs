//! `tracebridge debug` end to end against a fake RCL server that plays an
//! already running PowerView.

mod common;

use std::io::Write;
use std::process::Stdio;

use common::{FakeRcl, Project};

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Commands that would reset, start or flash the target.
fn assert_no_reset(commands: &[String]) {
    for command in commands {
        let upper = command.to_ascii_uppercase();
        assert!(
            !(upper.starts_with("SYSTEM.UP")
                || upper.starts_with("SYSTEM.MODE UP")
                || upper.starts_with("SYSTEM.MODE GO")
                || upper.starts_with("SYSTEM.RESET")
                || upper.starts_with("FLASH")),
            "unexpected command {command}"
        );
    }
}

fn halted(rcl: &FakeRcl) {
    let mut state = rcl.state();
    state.system_up = true;
    state.state_run = false;
}

fn function(rcl: &FakeRcl, expression: &str, result_type: u32, value: &str) {
    rcl.state()
        .functions
        .insert(expression.to_string(), (result_type, value.to_string()));
}

#[test]
fn debug_requires_a_running_powerview() {
    let project = Project::new(common::free_port(), "");
    let output = project.run(&["debug", "status"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no PowerView on RCL port")
            && stderr(&output).contains("tracebridge open"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn status_reads_state_without_resetting() {
    let rcl = FakeRcl::start();
    halted(&rcl);
    function(&rcl, "STATE.POWER()", 0x0001, "TRUE()");
    function(&rcl, "SYStem.CPU()", 0x0040, "CORTEXR52");
    function(&rcl, "Register(PP)", 0x0004, "0x1000");
    function(&rcl, "Register(CPSR)", 0x0004, "0x600001FA");
    let project = Project::new(rcl.port, "");
    let output = project.run(&["debug", "status"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("mode   up (11)"), "{text}");
    assert!(text.contains("state  halted"), "{text}");
    assert!(text.contains("pc     0x00001000"), "{text}");
    assert!(text.contains("hyp, T=1 (Thumb), masked: A I F"), "{text}");
    assert!(rcl.state().commands().is_empty());
}

#[test]
fn colours_only_on_request_when_piped() {
    let rcl = FakeRcl::start();
    halted(&rcl);
    let project = Project::new(rcl.port, "");
    let run = |args: &[&str], vars: &[(&str, &str)]| {
        let output = common::command_in(&project.root(), args)
            .envs(vars.iter().copied())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        stdout(&output)
    };
    // A pipe gets plain text.
    let plain = run(&["debug", "status"], &[]);
    assert!(plain.contains("state  halted"), "{plain}");
    assert!(!plain.contains('\x1b'), "{plain}");
    let forced = run(&["debug", "status"], &[("CLICOLOR_FORCE", "1")]);
    assert!(
        forced.contains("\x1b[36mstate\x1b[0m  \x1b[33mhalted\x1b[0m"),
        "{forced}"
    );
    let vars = [("CLICOLOR_FORCE", "1"), ("NO_COLOR", "1")];
    assert_eq!(run(&["debug", "status"], &vars), plain);
    // JSON is never coloured.
    let json = run(&["debug", "status", "--json"], &[("CLICOLOR_FORCE", "1")]);
    assert!(!json.contains('\x1b'), "{json}");
    serde_json::from_str::<serde_json::Value>(json.trim()).unwrap();
    // An error on stderr.
    let output = common::command_in(&project.root(), &["debug", "eval", "NOPE()"])
        .env("CLICOLOR_FORCE", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).starts_with("\x1b[1;31mtracebridge:\x1b[0m "),
        "{}",
        stderr(&output)
    );
}

#[test]
fn reg_json_output() {
    let rcl = FakeRcl::start();
    halted(&rcl);
    // No default PER file until PER.ReProgram (as after 'tracebridge open'),
    // although PER.FILENAME() already names the CPU's PER file.
    function(&rcl, "PER.FILENAME()", 0x0040, "perx.per");
    rcl.state().errors.insert(
        "PER.Set.CONDitions".into(),
        "No default peripheral file (PER.ReProgram) found.".into(),
    );
    function(&rcl, "PER.ADDRESS(\".CTRL\")", 0x0020, "C15:0x10010");
    function(
        &rcl,
        "ADDRESS.OFFSET(PER.ADDRESS(\".CTRL\"))",
        0x0004,
        "0x4004",
    );
    function(&rcl, "PER.VALUE(\".CTRL\")", 0x0004, "0x70E5");
    function(&rcl, "Data.Long(C15:0x1001)", 0x0004, "0x70E5");
    let project = Project::new(rcl.port, "");
    let output = project.run(&["debug", "reg", "CTRL", "MISSING", "--json"]);
    assert!(
        stderr(&output).contains("no default PER file loaded; running PER.ReProgram"),
        "{}",
        stderr(&output)
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let document: serde_json::Value = serde_json::from_str(stdout(&output).trim()).unwrap();
    assert_eq!(document["command"], "reg");
    let registers = &document["result"]["registers"];
    assert_eq!(registers[0]["address"], "C15:0x1001");
    assert_eq!(registers[0]["value"], 0x70E5);
    assert!(
        registers[1]["error"]
            .as_str()
            .unwrap()
            .starts_with("MISSING: not found in the PER file")
    );
    let commands = rcl.state().commands();
    assert_eq!(
        commands,
        ["PER.Set.CONDitions", "PER.ReProgram", "PER.Set.CONDitions"]
    );
    assert_eq!(registers[0]["address_checked"], true);
}

#[test]
fn eval_takes_the_rest_of_the_line() {
    let rcl = FakeRcl::start();
    function(&rcl, "Data.Long(AD:0x0)+1", 0x0004, "0x2A");
    let project = Project::new(rcl.port, "");
    let output = project.run(&["debug", "eval", "Data.Long(AD:0x0)+1"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "0x0000002A\n");
}

/// A little-endian ELF32 with one PT_LOAD segment whose VMA differs from its LMA.
fn elf(lma: u32, vma: u32, data: &[u8]) -> Vec<u8> {
    let mut header = vec![0u8; 52];
    header[..4].copy_from_slice(b"\x7fELF");
    header[4] = 1;
    header[5] = 1;
    header[6] = 1;
    header[0x1C..0x20].copy_from_slice(&52u32.to_le_bytes());
    header[0x2A..0x2C].copy_from_slice(&32u16.to_le_bytes());
    header[0x2C..0x2E].copy_from_slice(&1u16.to_le_bytes());
    for word in [
        1u32,
        84,
        vma,
        lma,
        data.len() as u32,
        data.len() as u32,
        6,
        4,
    ] {
        header.extend_from_slice(&word.to_le_bytes());
    }
    header.extend_from_slice(data);
    header
}

#[test]
fn verify_compares_at_the_load_address() {
    let rcl = FakeRcl::start();
    rcl.state().write_memory(0x0800_0400, b"DATA");
    rcl.state().write_memory(0x2000_0000, b"live");
    let project = Project::new(rcl.port, "");
    std::fs::write(
        project.root().join("build/demo.elf"),
        elf(0x0800_0400, 0x2000_0000, b"DATA"),
    )
    .unwrap();
    let output = project.run(&["debug", "verify"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).ends_with("\nmatch\n"),
        "{}",
        stdout(&output)
    );
    assert!(
        rcl.state()
            .log
            .iter()
            .any(|line| line == "read 0x8000400 4")
    );

    rcl.state().write_memory(0x0800_0401, b"?");
    let output = project.run(&["debug", "verify", "--json"]);
    assert_eq!(output.status.code(), Some(3));
    let document: serde_json::Value = serde_json::from_str(stdout(&output).trim()).unwrap();
    assert_eq!(document["result"]["match"], false);
    assert_eq!(document["result"]["first_difference"], 0x0800_0401u64);
}

#[test]
fn check_file_exit_codes() {
    let rcl = FakeRcl::start();
    function(&rcl, "Data.Long(AD:0x100)", 0x0004, "0x5");
    let project = Project::new(rcl.port, "");
    let file = project.root().join("checks.toml");
    std::fs::write(
        &file,
        "[[check]]\nname = \"word\"\nread = { addr = \"AD:0x100\" }\nexpect = { eq = 5 }\n\
         [[check]]\nname = \"other\"\nread = { addr = \"AD:0x100\" }\nexpect = { eq = 6 }\n\
         variants = [\"b\"]\n",
    )
    .unwrap();
    let output = project.run(&["debug", "check", "checks.toml"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("1 passed, 0 failed, 0 errors, 1 skipped"),
        "{}",
        stdout(&output)
    );
    let output = project.run(&["debug", "check", "checks.toml", "--variant", "b"]);
    assert_eq!(output.status.code(), Some(3));
    assert!(
        stdout(&output).contains("FAIL  other"),
        "{}",
        stdout(&output)
    );

    std::fs::write(&file, "[[check]]\nname = \"x\"\n").unwrap();
    let output = project.run(&["debug", "check", "checks.toml"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("read is missing"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn session_runs_commands_from_stdin() {
    let rcl = FakeRcl::start();
    halted(&rcl);
    let project = Project::new(rcl.port, "");
    let mut child = common::command_in(&project.root(), &["debug"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"bogus\neval NOPE()\ngo\nbreak\nquit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("halted (now: up, halted)"), "{text}");
    assert!(stderr(&output).contains("unrecognized subcommand 'bogus'"));
    let commands = rcl.state().commands();
    assert_eq!(commands, ["Go", "Break"]);
    assert_no_reset(&commands);
    assert!(project.root().join(".tracebridge/debug_history").is_file());
}

#[test]
fn help_needs_no_powerview_and_no_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let output = common::run_in(dir.path(), &["debug", "--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("[R] Debugger mode"));
    let output = common::run_in(dir.path(), &["debug", "help", "check"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("--dry-run"));
    let output = common::run_in(dir.path(), &["debug", "up"]);
    assert_eq!(output.status.code(), Some(2));
}
