//! `tracebridge open/load/flash` end to end against a fake RCL server that
//! plays the role of an already running PowerView.

mod common;

use common::{FakeRcl, Project};

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn open_reuses_running_powerview() {
    let rcl = FakeRcl::start();
    let project = Project::new(rcl.port, "");
    let output = project.run(&["open"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains(&format!("reusing PowerView on RCL port {}", rcl.port)));
    assert!(rcl.state().commands().is_empty());
}

#[test]
fn load_attaches_loads_symbols_and_runs() {
    let rcl = FakeRcl::start();
    let project = Project::new(rcl.port, "");
    let output = project.run(&["load"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let elf = project.root().join("build/demo.elf");
    assert_eq!(
        rcl.state().commands(),
        [
            "SYStem.Mode Down".to_string(),
            "SYStem.CPU CORTEXM4".into(),
            "CORE.ASSIGN 1.".into(),
            "SYStem.Mode Attach".into(),
            format!("Data.LOAD.Elf \"{}\" /NoCODE", elf.display()),
            "List.auto".into(),
            "Go".into(),
            "ECHO \"tracebridge: symbols loaded for demo\"".into(),
        ]
    );
    let text = stdout(&output);
    assert!(text.contains("loading symbols from"), "{text}");
    assert!(text.contains("symbols loaded, target running"), "{text}");
}

#[test]
fn flash_runs_the_project_script_then_programs() {
    let rcl = FakeRcl::start();
    let project = Project::new(rcl.port, "");
    let output = project.run(&["flash"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let root = project.root();
    let log = rcl.state().log.clone();
    let script = root.join("flash.cmm");
    assert_eq!(log[0], "fnc PRACTICE.SD()");
    assert_eq!(
        log[1],
        format!("cmd DO \"{}\" PREPAREONLY", script.display())
    );
    let commands = rcl.state().commands();
    assert!(commands.contains(&"FLASH.ReProgram ALL /Erase".to_string()));
    let elf = root.join("build/demo.elf");
    let load = commands
        .iter()
        .position(|c| *c == format!("Data.LOAD.Elf \"{}\"", elf.display()))
        .unwrap();
    assert_eq!(commands[load + 1], "FLASH.ReProgram OFF");
    assert!(stdout(&output).contains("flashed, symbols loaded, target running"));
}

#[test]
fn flash_failure_is_reported_with_the_command() {
    let rcl = FakeRcl::start();
    rcl.state().failing.push("FLASH.ReProgram ALL".into());
    let project = Project::new(rcl.port, "");
    let output = project.run(&["flash"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "tracebridge: TRACE32 flash failed: command failed (command: FLASH.ReProgram ALL /Erase)\n"
    );
}

#[test]
fn port_in_use_by_something_else_is_rejected() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            drop(stream);
        }
    });
    let project = Project::new(port, "");
    let output = project.run(&["open"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains(&format!(
            "port {port} is open but is not a usable TRACE32 RCL endpoint"
        )),
        "{}",
        stderr(&output)
    );
}

#[test]
fn missing_powerview_is_reported_when_nothing_listens() {
    let project = Project::new(common::free_port(), "");
    let output = project.run(&["open"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).starts_with("tracebridge: PowerView not found: "));
}

#[test]
fn flash_script_is_chosen_by_chip_from_the_library() {
    let rcl = FakeRcl::start();
    let project = Project::new(rcl.port, "");
    // The test HOME is the project directory, so the library lives inside it.
    let library = project.root().join(".config/tracebridge/flash");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::write(
        library.join("board.cmm"),
        "; @Chip: MYCHIP*\n; DO board [PREPAREONLY]\n",
    )
    .unwrap();
    let output = project.run(&["flash", "--chip", "MYCHIP-A"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("(library script for MYCHIP-A, @Chip MYCHIP*)"),
        "{}",
        stdout(&output)
    );
    assert_eq!(
        rcl.state().log[1],
        format!(
            "cmd DO \"{}\" PREPAREONLY",
            library.join("board.cmm").display()
        )
    );

    let output = project.run(&["flash", "--chip", "OTHER"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no flash script found for chip OTHER"));
}
