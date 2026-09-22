//! Command-line behaviour of the tracebridge binary.

use std::path::Path;
use std::process::{Command, Output};

fn tracebridge(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tracebridge"))
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn version_includes_git_hash() {
    let dir = tempfile::tempdir().unwrap();
    let output = tracebridge(dir.path(), &["--version"]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.starts_with("tracebridge 0.1.0 ("), "{text}");
    assert!(text.trim_end().ends_with(')'), "{text}");
}

#[test]
fn init_then_config_from_a_subdirectory() {
    let dir = tempfile::tempdir().unwrap();
    let output = tracebridge(dir.path(), &["init"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(dir.path().join("trace32.toml").is_file());

    let nested = dir.path().join("src/app");
    std::fs::create_dir_all(&nested).unwrap();
    let output = tracebridge(&nested, &["config"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("configuration: "), "{text}");
    assert!(text.contains("MISSING ELF="), "{text}");
    assert!(text.contains("ports   RCL 20000, DAP 58870"), "{text}");
}

#[test]
fn init_refuses_to_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("trace32.toml"), "x").unwrap();
    let output = tracebridge(dir.path(), &["init"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).starts_with("tracebridge: "));
    assert!(stderr(&output).contains("refusing to overwrite"));
}

#[test]
fn missing_configuration_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let output = tracebridge(dir.path(), &["config"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no trace32.toml found in"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn explicit_config_is_used() {
    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("elsewhere");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("custom.toml"), "[project]\nprogram = \"x\"\n").unwrap();
    std::fs::write(
        dir.path().join("trace32.toml"),
        "[project]\nprogram = \"y\"\n",
    )
    .unwrap();
    let output = tracebridge(dir.path(), &["--config", "elsewhere/custom.toml", "config"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("custom.toml"));
}

#[test]
fn usage_errors_exit_with_2() {
    let dir = tempfile::tempdir().unwrap();
    let output = tracebridge(dir.path(), &["bogus"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn configuration_errors_exit_with_1() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("trace32.toml"),
        "[project]\nprogram = \"\"\n",
    )
    .unwrap();
    let output = tracebridge(dir.path(), &["config"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "tracebridge: project.program is empty in trace32.toml\n"
    );
}
