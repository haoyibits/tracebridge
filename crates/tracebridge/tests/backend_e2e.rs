//! Public backend API against the existing fake PowerView, without a CLI subprocess.
mod common;

use common::{FakeRcl, Project as Fixture};
use tracebridge::backend::{CheckOptions, Project, Session};

fn session(rcl: &FakeRcl, fixture: &Fixture) -> Session {
    rcl.state().system_up = true;
    Session::connect(Project::load(&fixture.root().join("trace32.toml")).unwrap()).unwrap()
}

#[test]
fn same_session_returns_typed_check_failures_and_bulk_ram() {
    let rcl = FakeRcl::start();
    let fixture = Fixture::new(rcl.port, "");
    let checks = fixture.root().join("checks.toml");
    std::fs::write(
        &checks,
        "[[check]]\nname='value'\nread={addr='AD:0x20000000'}\nexpect={eq=7}\n",
    )
    .unwrap();
    rcl.state()
        .functions
        .insert("Data.Long(AD:0x20000000)".into(), (4, "0x6".into()));
    let expected: Vec<u32> = (0..300).collect();
    let bytes: Vec<u8> = expected.iter().flat_map(|v| v.to_le_bytes()).collect();
    rcl.state().write_memory(0x20000000, &bytes);
    let mut backend = session(&rcl, &fixture);
    let report = backend
        .check(
            &checks,
            &CheckOptions {
                variant: None,
                halt: false,
                dry_run: false,
            },
        )
        .unwrap();
    assert!(!report.passed());
    assert_eq!(report.exit_code(), 3);
    assert_eq!(report.results.len(), 1);
    assert_eq!(backend.read_ram_words(0x20000000, 300).unwrap(), expected);
    assert!(backend.read_ram_words(0x20000001, 300).is_err());
    backend.write_ram_word(0x20001000, 0x12345678).unwrap();
    assert_eq!(
        rcl.state().read_memory(0x20001000, 4),
        0x12345678u32.to_le_bytes()
    );
    let log = rcl.state().log.clone();
    assert_eq!(log.iter().filter(|s| s.starts_with("read ")).count(), 2);
    assert!(
        !log.iter()
            .any(|s| s.starts_with("cmd FLASH") || s.starts_with("cmd SYStem"))
    );
}

#[test]
fn library_flash_uses_the_existing_sequence_and_stops_on_failure() {
    let rcl = FakeRcl::start();
    let fixture = Fixture::new(rcl.port, "");
    let mut backend = session(&rcl, &fixture);
    rcl.state().failing.push("Data.LOAD.Elf".into());
    assert!(
        backend
            .flash()
            .unwrap_err()
            .to_string()
            .contains("command failed")
    );
    let commands = rcl.state().commands();
    let load = commands
        .iter()
        .position(|s| s.starts_with("Data.LOAD.Elf"))
        .unwrap();
    assert_eq!(commands[load + 1], "FLASH.ReProgram OFF");
    assert_eq!(commands.len(), load + 2);
    assert_eq!(commands.iter().filter(|s| s.starts_with("DO ")).count(), 1);
}

#[test]
fn project_loading_is_offline_and_empty_images_clear_defaults() {
    let fixture = Fixture::new(common::free_port(), "");
    let mut project = Project::load(&fixture.root().join("trace32.toml")).unwrap();
    project.images(fixture.root().join("absent.elf"), vec![], "plain".into());
    assert!(
        project
            .evidence_inputs()
            .unwrap_err()
            .to_string()
            .contains("ELF")
    );
}

#[test]
fn offline_selection_rejects_typos_instead_of_checking_only_common_criteria() {
    let fixture = Fixture::new(common::free_port(), "");
    let path = fixture.root().join("checks.toml");
    std::fs::write(&path, "[[check]]\nname='common'\nread={addr='AD:0x20000000'}\nexpect={eq=7}\n[[check]]\nname='specific'\nread={addr='AD:0x20000004'}\nexpect={eq=8}\nvariants=['default']\n").unwrap();
    assert_eq!(
        tracebridge::backend::check_names(&path, Some("default")).unwrap(),
        ["common", "specific"]
    );
    assert!(tracebridge::backend::check_names(&path, Some("defualt")).is_err());
}
