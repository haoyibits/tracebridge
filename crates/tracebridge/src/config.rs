//! trace32.toml loading (config.py).
//!
//! Differences from the Python tool: the configuration file is found by
//! searching upwards from the working directory, the project root is the
//! directory that contains it, every relative path is resolved against that
//! directory, and runtime files live in `<project>/.tracebridge/`.

use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::errors::Result;
use crate::pycompat::{Env, expand_path, parse_decimal, parse_int_auto, resolve, shlex_split};
use crate::{bail, bridge_error};

pub const CONFIG_FILE_NAME: &str = "trace32.toml";
pub const RUN_DIR_NAME: &str = ".tracebridge";

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub config_file: PathBuf,
    /// Directory containing trace32.toml; the base of every relative path.
    pub config_dir: PathBuf,
    pub run_dir: PathBuf,
    pub project_dir: PathBuf,
    pub program: String,
    pub elf: PathBuf,
    pub cpu: String,
    pub cores: String,
    pub mem_access: String,
    pub jtag_clock: String,
    pub dual_port: String,
    pub rtos_config: String,
    pub rtos_menu: String,
    pub rtos_show_tasks: bool,
    pub flash_chip: String,
    pub flash_script: String,
    pub flash_args: Vec<String>,
    pub t32_sys: PathBuf,
    pub t32_host: String,
    pub t32_binary: PathBuf,
    pub t32_config: PathBuf,
    pub debug_adapter: PathBuf,
    pub rcl_port: u16,
    pub dap_port: u16,
    pub dap_backend_port: u16,
    pub dap_backend_timeout: u64,
    pub operation_timeout: u64,
    pub rtt_symbol: String,
    pub rtt_control_block_address: Option<u64>,
    pub rtt_poll_interval: f64,
}

/// Find the configuration: `--config` if given, otherwise the nearest
/// trace32.toml in `cwd` or one of its parents.
pub fn find_config_file(explicit: Option<&Path>, cwd: &Path, env: &Env) -> Result<PathBuf> {
    if let Some(path) = explicit {
        let expanded = crate::pycompat::expanduser(&path.to_string_lossy(), env);
        return Ok(resolve(Path::new(&expanded), cwd));
    }
    let mut directory = Some(cwd);
    while let Some(dir) = directory {
        let candidate = dir.join(CONFIG_FILE_NAME);
        if candidate.is_file() {
            return Ok(resolve(&candidate, cwd));
        }
        directory = dir.parent();
    }
    bail!(
        "no {CONFIG_FILE_NAME} found in {} or any parent directory; \
         run 'tracebridge init' to create one, or pass --config",
        cwd.display()
    )
}

fn table<'a>(document: &'a Table, name: &str) -> Result<Option<&'a Table>> {
    match document.get(name) {
        None => Ok(None),
        Some(Value::Table(table)) => Ok(Some(table)),
        Some(_) => bail!("[{name}] in trace32.toml must be a table"),
    }
}

fn text(table: Option<&Table>, key: &str, default: &str) -> Result<String> {
    match table.and_then(|t| t.get(key)) {
        None => Ok(default.to_string()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => bail!("{key} must be a string"),
    }
}

fn integer(table: Option<&Table>, key: &str, default: i64) -> Result<i64> {
    match table.and_then(|t| t.get(key)) {
        None => Ok(default),
        Some(Value::Integer(value)) => Ok(*value),
        Some(_) => bail!("{key} must be an integer"),
    }
}

fn number(table: Option<&Table>, key: &str, default: f64) -> Result<f64> {
    match table.and_then(|t| t.get(key)) {
        None => Ok(default),
        Some(Value::Integer(value)) => Ok(*value as f64),
        Some(Value::Float(value)) => Ok(*value),
        Some(_) => bail!("{key} must be a number"),
    }
}

fn boolean(table: Option<&Table>, key: &str, default: bool) -> Result<bool> {
    match table.and_then(|t| t.get(key)) {
        None => Ok(default),
        Some(Value::Boolean(value)) => Ok(*value),
        Some(_) => bail!("{key} must be a boolean"),
    }
}

fn string_list(table: Option<&Table>, key: &str) -> Result<Vec<String>> {
    match table.and_then(|t| t.get(key)) {
        None => Ok(Vec::new()),
        Some(Value::String(value)) => {
            shlex_split(value).map_err(|error| bridge_error!("cannot parse {key}: {error}"))
        }
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(value) => Ok(value.clone()),
                _ => Err(bridge_error!("{key} must be an array of strings")),
            })
            .collect(),
        Some(_) => bail!("{key} must be an array of strings"),
    }
}

/// `_env_text`: an unset or empty variable falls back.
fn env_text(env: &Env, name: &str, fallback: String) -> String {
    match env.get(name) {
        Some(value) if !value.is_empty() => value.clone(),
        _ => fallback,
    }
}

/// `_env_override`: a set variable wins even when empty.
fn env_override(env: &Env, name: &str, fallback: String) -> String {
    env.get(name).cloned().unwrap_or(fallback)
}

/// `_env_integer`: an unset or empty variable falls back.
fn env_integer(env: &Env, name: &str, fallback: i64) -> Result<i64> {
    match env.get(name) {
        Some(value) if !value.is_empty() => parse_decimal(value)
            .ok_or_else(|| bridge_error!("{name} environment override must be an integer")),
        _ => Ok(fallback),
    }
}

/// `_env_arguments`: a set variable is split like a shell command line.
fn env_arguments(env: &Env, name: &str, fallback: Vec<String>) -> Result<Vec<String>> {
    match env.get(name) {
        Some(value) => {
            shlex_split(value).map_err(|error| bridge_error!("cannot parse {name}: {error}"))
        }
        None => Ok(fallback),
    }
}

/// `_host_default`.
pub fn host_default() -> Result<String> {
    match std::env::consts::OS {
        "macos" => Ok("macosx64".into()),
        "linux" => Ok("linux64".into()),
        other => {
            bail!("cannot auto-detect the TRACE32 host directory on {other}; set trace32.host")
        }
    }
}

fn home(env: &Env) -> PathBuf {
    PathBuf::from(crate::pycompat::expanduser("~", env))
}

/// Load and validate the configuration (`load_config`).
pub fn load_config(config_file: &Path, env: &Env) -> Result<Config> {
    let source = match std::fs::read(config_file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("config not found: {}", config_file.display())
        }
        Err(error) => bail!("cannot read {}: {error}", config_file.display()),
    };
    let source = String::from_utf8(source)
        .map_err(|error| bridge_error!("cannot parse {}: {error}", config_file.display()))?;
    let document: Table = source.parse().map_err(|error: toml::de::Error| {
        let location = error
            .span()
            .map(|span| {
                let before = &source[..span.start.min(source.len())];
                let line = before.matches('\n').count() + 1;
                let column = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
                format!(" (at line {line}, column {column})")
            })
            .unwrap_or_default();
        bridge_error!(
            "cannot parse {}: {}{location}",
            config_file.display(),
            error.message().trim_end()
        )
    })?;

    let project = table(&document, "project")?;
    let target = table(&document, "target")?;
    let flash = table(&document, "flash")?;
    let rtos = table(&document, "rtos")?;
    let trace32 = table(&document, "trace32")?;
    let rtt = table(&document, "rtt")?;

    let config_dir = config_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"));
    if project.is_some_and(|p| p.contains_key("root")) {
        bail!(
            "project.root is no longer supported; the project root is the directory \
             containing trace32.toml ({}); remove it",
            config_dir.display()
        );
    }
    let project_dir = match env.get("PROJECT_ROOT").filter(|v| !v.is_empty()) {
        Some(value) => {
            let path = resolve(Path::new(&expand_path(value, env)), &config_dir);
            if !path.is_dir() {
                bail!(
                    "PROJECT_ROOT does not resolve to a directory: {}",
                    path.display()
                );
            }
            path
        }
        None => config_dir.clone(),
    };
    let elf = resolve(
        Path::new(&expand_path(
            &env_text(env, "ELF", text(project, "elf", "")?),
            env,
        )),
        &project_dir,
    );

    let toml_sys = text(trace32, "sys", "")?;
    let sys_default = if toml_sys.is_empty() {
        home(env).join("t32").to_string_lossy().into_owned()
    } else {
        toml_sys
    };
    let sys_value = env_text(env, "T32_SYS", env_text(env, "T32SYS", sys_default));
    let t32_sys = resolve(Path::new(&expand_path(&sys_value, env)), &config_dir);
    let mut t32_host = env_text(env, "T32_HOST", text(trace32, "host", "")?);
    if t32_host.is_empty() {
        t32_host = host_default()?;
    }
    let executable = env_text(env, "T32_EXE", text(trace32, "executable", "t32marm-qt")?);
    let binary_value = env_text(env, "T32_BIN", text(trace32, "binary", "")?);
    let config_value = env_text(env, "T32_CONFIG", text(trace32, "config", "")?);
    let adapter_value = env_text(
        env,
        "T32_DEBUG_ADAPTER",
        text(trace32, "debug_adapter", "")?,
    );
    let explicit = |value: &str| resolve(Path::new(&expand_path(value, env)), &config_dir);

    let t32_binary = if binary_value.is_empty() {
        t32_sys.join("bin").join(&t32_host).join(&executable)
    } else {
        explicit(&binary_value)
    };
    let t32_config = if config_value.is_empty() {
        t32_sys.join("config.t32")
    } else {
        explicit(&config_value)
    };
    let debug_adapter = if adapter_value.is_empty() {
        t32_sys
            .join("demo/env/vscode/bin")
            .join(&t32_host)
            .join("t32debugadapter")
    } else {
        explicit(&adapter_value)
    };

    let address_text = text(rtt, "control_block_address", "")?;
    let rtt_control_block_address = if address_text.is_empty() {
        None
    } else {
        Some(
            parse_int_auto(&address_text)
                .and_then(|value| u64::try_from(value).ok())
                .ok_or_else(|| {
                    bridge_error!(
                        "rtt.control_block_address must be empty or an integer such as 0x20000000"
                    )
                })?,
        )
    };

    let program = env_override(env, "PROGRAM_NAME", text(project, "program", "")?);
    let cpu = env_override(env, "T32_CPU", text(target, "cpu", "")?);
    let cores = env_override(env, "T32_CORES", text(target, "cores", "1.")?);
    let mem_access = env_override(env, "T32_MEMACCESS", text(target, "mem_access", "")?);
    let jtag_clock = env_override(env, "T32_JTAG_CLOCK", text(target, "jtag_clock", "")?);
    let dual_port = env_override(env, "T32_DUALPORT", text(target, "dual_port", "")?);
    let rtos_config = text(rtos, "config", "")?;
    let rtos_menu = text(rtos, "menu", "")?;
    let rtos_show_tasks = boolean(rtos, "show_tasks", false)?;
    let flash_chip = env_text(env, "T32_FLASH_CHIP", text(flash, "chip", "")?);
    let flash_script = env_override(env, "T32_FLASH_SCRIPT", text(flash, "script", "")?);
    let flash_args = env_arguments(env, "T32_FLASH_ARGS", string_list(flash, "args")?)?;
    let rcl_port = env_integer(env, "T32_RCL_PORT", integer(trace32, "rcl_port", 20000)?)?;
    let dap_port = env_integer(env, "T32_DAP_PORT", integer(trace32, "dap_port", 58870)?)?;
    let dap_backend_port = env_integer(
        env,
        "T32_DAP_BACKEND_PORT",
        integer(trace32, "dap_backend_port", 58871)?,
    )?;
    let dap_backend_timeout = env_integer(
        env,
        "T32_DAP_BACKEND_TIMEOUT",
        integer(trace32, "dap_backend_timeout", 30)?,
    )?;
    let operation_timeout = env_integer(
        env,
        "T32_TIMEOUT",
        integer(trace32, "operation_timeout", 600)?,
    )?;
    let rtt_symbol = env_text(env, "RTT_SYMBOL", text(rtt, "symbol", "_SEGGER_RTT")?);
    let rtt_poll_interval = number(rtt, "poll_interval", 0.02)?;

    // validate_common
    if program.is_empty() {
        bail!("project.program is empty in trace32.toml");
    }
    let port = |name: &str, value: i64| -> Result<u16> {
        if (1..=65535).contains(&value) {
            Ok(value as u16)
        } else {
            bail!("{name} must be between 1 and 65535")
        }
    };
    let rcl_port = port("trace32.rcl_port", rcl_port)?;
    let dap_port = port("trace32.dap_port", dap_port)?;
    let dap_backend_port = port("trace32.dap_backend_port", dap_backend_port)?;
    if dap_port == dap_backend_port {
        bail!("trace32.dap_port and trace32.dap_backend_port must be different");
    }
    if dap_backend_timeout <= 0 {
        bail!("trace32.dap_backend_timeout must be positive");
    }
    if operation_timeout <= 0 {
        bail!("trace32.operation_timeout must be positive");
    }
    if rtt_poll_interval <= 0.0 || rtt_poll_interval.is_nan() {
        bail!("rtt.poll_interval must be positive");
    }

    let config = Config {
        config_file: config_file.to_path_buf(),
        run_dir: config_dir.join(RUN_DIR_NAME),
        config_dir,
        project_dir,
        program,
        elf,
        cpu,
        cores,
        mem_access,
        jtag_clock,
        dual_port,
        rtos_config,
        rtos_menu,
        rtos_show_tasks,
        flash_chip,
        flash_script,
        flash_args,
        t32_sys,
        t32_host,
        t32_binary,
        t32_config,
        debug_adapter,
        rcl_port,
        dap_port,
        dap_backend_port,
        dap_backend_timeout: dap_backend_timeout as u64,
        operation_timeout: operation_timeout as u64,
        rtt_symbol,
        rtt_control_block_address,
        rtt_poll_interval,
    };
    config.check_command_values()?;
    Ok(config)
}

impl Config {
    /// `resolved_flash_script`: TRACE32 paths (`~~...`) stay as they are, other
    /// paths are resolved against the project root.
    pub fn resolved_flash_script(&self) -> String {
        if self.flash_script.is_empty() || self.flash_script.starts_with("~~") {
            return self.flash_script.clone();
        }
        resolve(Path::new(&self.flash_script), &self.config_dir)
            .to_string_lossy()
            .into_owned()
    }

    /// `flash_script_exists`.
    pub fn flash_script_exists(&self) -> bool {
        let path = self.resolved_flash_script();
        if path.is_empty() {
            return false;
        }
        let practice = path == "~~" || path.starts_with("~~/");
        practice || Path::new(&path).is_file()
    }

    /// The last part of `validate_common`: values embedded in TRACE32 commands
    /// must not break the quoting.
    fn check_command_values(&self) -> Result<()> {
        let values = [
            (
                "config file",
                self.config_file.to_string_lossy().into_owned(),
            ),
            ("run directory", self.run_dir.to_string_lossy().into_owned()),
            ("project.program", self.program.clone()),
            ("project.elf", self.elf.to_string_lossy().into_owned()),
            ("target.cpu", self.cpu.clone()),
            ("target.cores", self.cores.clone()),
            ("target.mem_access", self.mem_access.clone()),
            ("target.jtag_clock", self.jtag_clock.clone()),
            ("target.dual_port", self.dual_port.clone()),
            ("rtos.config", self.rtos_config.clone()),
            ("rtos.menu", self.rtos_menu.clone()),
            ("flash.script", self.resolved_flash_script()),
            ("flash.args", self.flash_args.join(" ")),
        ];
        for (name, value) in values {
            if value.contains(['"', '\n', '\r']) {
                bail!("{name} contains characters unsafe for TRACE32 commands");
            }
        }
        Ok(())
    }

    /// Create the run directory (with a .gitignore that ignores everything).
    pub fn ensure_run_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.run_dir)
            .map_err(|error| bridge_error!("cannot create {}: {error}", self.run_dir.display()))?;
        let ignore = self.run_dir.join(".gitignore");
        if !ignore.exists() {
            std::fs::write(&ignore, "*\n")
                .map_err(|error| bridge_error!("cannot write {}: {error}", ignore.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const CONFIG: &str = r#"
[project]
program = "demo"
elf = "build/demo.elf"

[target]
cpu = "CORTEXM4"
cores = "1."
mem_access = "AXI"
jtag_clock = "10MHz"
dual_port = "ON"

[flash]
script = "flash.cmm"
args = ["CPU=DEMO", "DUALPORT=1"]

[trace32]
sys = "fake-t32"
rcl_port = 20000
dap_port = 58870
dap_backend_port = 58871

[rtt]
symbol = "_SEGGER_RTT"
control_block_address = "0x20001000"
poll_interval = 0.05
"#;

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    impl Fixture {
        fn new(source: &str) -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let root = fs::canonicalize(dir.path()).unwrap().join("project");
            Fixture::write(&root, source);
            Fixture { _dir: dir, root }
        }

        fn write(root: &Path, source: &str) {
            fs::create_dir_all(root.join("build")).unwrap();
            fs::write(root.join("flash.cmm"), "").unwrap();
            fs::write(root.join(CONFIG_FILE_NAME), source).unwrap();
        }

        fn file(&self) -> PathBuf {
            self.root.join(CONFIG_FILE_NAME)
        }

        fn load(&self, env: &[(&str, &str)]) -> Result<Config> {
            let mut vars: Env = [("HOME".to_string(), "/nonexistent-tb-home".to_string())].into();
            vars.extend(env.iter().map(|(k, v)| (k.to_string(), v.to_string())));
            load_config(&self.file(), &vars)
        }

        fn replace(&self, from: &str, to: &str) {
            fs::write(self.file(), CONFIG.replace(from, to)).unwrap();
        }
    }

    // test_config.py: test_loads_typed_configuration_and_resolves_paths
    #[test]
    fn loads_typed_configuration_and_resolves_paths() {
        let fixture = Fixture::new(CONFIG);
        let config = fixture.load(&[]).unwrap();
        assert_eq!(config.project_dir, fixture.root);
        assert_eq!(config.config_dir, fixture.root);
        assert_eq!(config.run_dir, fixture.root.join(".tracebridge"));
        assert_eq!(config.elf, fixture.root.join("build/demo.elf"));
        assert_eq!(config.rtt_control_block_address, Some(0x2000_1000));
        assert_eq!(config.flash_args, ["CPU=DEMO", "DUALPORT=1"]);
        assert_eq!(
            config.resolved_flash_script(),
            fixture.root.join("flash.cmm").to_string_lossy()
        );
        assert!(config.flash_script_exists());
        assert_eq!(config.t32_sys, fixture.root.join("fake-t32"));
        assert_eq!(config.rtt_poll_interval, 0.05);
    }

    // test_config.py: test_environment_overrides_trace32_installation
    #[test]
    fn environment_overrides_trace32_installation() {
        let fixture = Fixture::new(CONFIG);
        let override_dir = fixture.root.join("override");
        let config = fixture
            .load(&[
                ("T32SYS", override_dir.to_str().unwrap()),
                ("T32_RCL_PORT", "21000"),
                ("PROGRAM_NAME", "override-program"),
            ])
            .unwrap();
        assert_eq!(config.t32_sys, override_dir);
        assert_eq!(config.rcl_port, 21000);
        assert_eq!(config.program, "override-program");
    }

    // test_config.py: test_paths_with_spaces_are_supported
    #[test]
    fn paths_with_spaces_are_supported() {
        let fixture = Fixture::new(CONFIG);
        let spaced = fixture.root.join("tool kit");
        Fixture::write(&spaced, CONFIG);
        let config = load_config(&spaced.join(CONFIG_FILE_NAME), &Env::new()).unwrap();
        assert_eq!(config.config_dir, spaced);
        assert!(config.elf.to_string_lossy().contains("tool kit"));
    }

    // test_config.py: test_loads_optional_rtos_configuration
    #[test]
    fn loads_optional_rtos_configuration() {
        let fixture = Fixture::new(&format!(
            "{CONFIG}\n[rtos]\nconfig = \"~~/demo/freertos.t32\"\nmenu = \"~~/demo/freertos.men\"\nshow_tasks = true\n"
        ));
        let config = fixture.load(&[]).unwrap();
        assert_eq!(config.rtos_config, "~~/demo/freertos.t32");
        assert!(config.rtos_show_tasks);
    }

    // test_config.py: test_rejects_unsafe_trace32_value
    #[test]
    fn rejects_unsafe_trace32_value() {
        let fixture = Fixture::new(CONFIG);
        fixture.replace("program = \"demo\"", "program = 'bad\"name'");
        let error = fixture.load(&[]).unwrap_err();
        assert!(error.0.contains("unsafe for TRACE32"), "{error}");
    }

    #[test]
    fn defaults_follow_python() {
        let fixture = Fixture::new("[project]\nprogram = \"app\"\n");
        let config = fixture.load(&[]).unwrap();
        assert_eq!(config.cores, "1.");
        assert_eq!(config.t32_sys, PathBuf::from("/nonexistent-tb-home/t32"));
        let host = host_default().unwrap();
        assert_eq!(
            config.t32_binary,
            PathBuf::from(format!("/nonexistent-tb-home/t32/bin/{host}/t32marm-qt"))
        );
        assert_eq!(
            config.t32_config,
            PathBuf::from("/nonexistent-tb-home/t32/config.t32")
        );
        assert_eq!(
            config.debug_adapter,
            PathBuf::from(format!(
                "/nonexistent-tb-home/t32/demo/env/vscode/bin/{host}/t32debugadapter"
            ))
        );
        assert_eq!(
            (config.rcl_port, config.dap_port, config.dap_backend_port),
            (20000, 58870, 58871)
        );
        assert_eq!(
            (config.dap_backend_timeout, config.operation_timeout),
            (30, 600)
        );
        assert_eq!(config.rtt_symbol, "_SEGGER_RTT");
        assert_eq!(config.rtt_control_block_address, None);
        assert_eq!(config.rtt_poll_interval, 0.02);
        assert_eq!(config.elf, fixture.root);
        assert!(!config.flash_script_exists());
    }

    #[test]
    fn env_text_ignores_empty_values_but_env_override_does_not() {
        let fixture = Fixture::new(CONFIG);
        let config = fixture
            .load(&[
                ("ELF", ""),
                ("T32_CPU", ""),
                ("RTT_SYMBOL", ""),
                ("T32_RCL_PORT", ""),
            ])
            .unwrap();
        assert_eq!(config.elf, fixture.root.join("build/demo.elf"));
        assert_eq!(config.cpu, "");
        assert_eq!(config.rtt_symbol, "_SEGGER_RTT");
        assert_eq!(config.rcl_port, 20000);
        let error = fixture.load(&[("PROGRAM_NAME", "")]).unwrap_err();
        assert_eq!(error.0, "project.program is empty in trace32.toml");
    }

    #[test]
    fn t32_sys_takes_precedence_over_t32sys() {
        let fixture = Fixture::new(CONFIG);
        let config = fixture
            .load(&[("T32_SYS", "/opt/a"), ("T32SYS", "/opt/b")])
            .unwrap();
        assert_eq!(config.t32_sys, PathBuf::from("/opt/a"));
        let config = fixture
            .load(&[("T32_SYS", ""), ("T32SYS", "/opt/b")])
            .unwrap();
        assert_eq!(config.t32_sys, PathBuf::from("/opt/b"));
    }

    #[test]
    fn flash_args_environment_is_split_like_a_shell() {
        let fixture = Fixture::new(CONFIG);
        let config = fixture.load(&[("T32_FLASH_ARGS", "A=1 'B C'")]).unwrap();
        assert_eq!(config.flash_args, ["A=1", "B C"]);
        let config = fixture.load(&[("T32_FLASH_ARGS", "")]).unwrap();
        assert!(config.flash_args.is_empty());
        let error = fixture.load(&[("T32_FLASH_ARGS", "'x")]).unwrap_err();
        assert_eq!(error.0, "cannot parse T32_FLASH_ARGS: No closing quotation");
    }

    #[test]
    fn integer_overrides_are_validated() {
        let fixture = Fixture::new(CONFIG);
        let error = fixture.load(&[("T32_DAP_PORT", "abc")]).unwrap_err();
        assert_eq!(
            error.0,
            "T32_DAP_PORT environment override must be an integer"
        );
        let error = fixture.load(&[("T32_DAP_PORT", "58871")]).unwrap_err();
        assert_eq!(
            error.0,
            "trace32.dap_port and trace32.dap_backend_port must be different"
        );
        let error = fixture.load(&[("T32_RCL_PORT", "70000")]).unwrap_err();
        assert_eq!(error.0, "trace32.rcl_port must be between 1 and 65535");
        let error = fixture.load(&[("T32_TIMEOUT", "0")]).unwrap_err();
        assert_eq!(error.0, "trace32.operation_timeout must be positive");
    }

    #[test]
    fn type_errors_name_the_key() {
        let fixture = Fixture::new(CONFIG);
        fixture.replace("rcl_port = 20000", "rcl_port = \"20000\"");
        assert_eq!(
            fixture.load(&[]).unwrap_err().0,
            "rcl_port must be an integer"
        );
        fixture.replace("cpu = \"CORTEXM4\"", "cpu = 4");
        assert_eq!(fixture.load(&[]).unwrap_err().0, "cpu must be a string");
        fixture.replace("args = [\"CPU=DEMO\", \"DUALPORT=1\"]", "args = [1]");
        assert_eq!(
            fixture.load(&[]).unwrap_err().0,
            "args must be an array of strings"
        );
        fixture.replace("poll_interval = 0.05", "poll_interval = true");
        assert_eq!(
            fixture.load(&[]).unwrap_err().0,
            "poll_interval must be a number"
        );
        fs::write(
            fixture.file(),
            format!("rtt = 1\n{}", CONFIG.replace("[rtt]", "[other]")),
        )
        .unwrap();
        assert_eq!(
            fixture.load(&[]).unwrap_err().0,
            "[rtt] in trace32.toml must be a table"
        );
        fixture.replace("0x20001000", "zz");
        assert_eq!(
            fixture.load(&[]).unwrap_err().0,
            "rtt.control_block_address must be empty or an integer such as 0x20000000"
        );
    }

    #[test]
    fn flash_args_string_is_split() {
        let fixture = Fixture::new(CONFIG);
        fixture.replace(
            "args = [\"CPU=DEMO\", \"DUALPORT=1\"]",
            "args = \"X=1 Y=2\"",
        );
        assert_eq!(fixture.load(&[]).unwrap().flash_args, ["X=1", "Y=2"]);
    }

    #[test]
    fn project_root_key_is_rejected() {
        let fixture = Fixture::new(CONFIG);
        fixture.replace("[project]", "[project]\nroot = \"..\"");
        let error = fixture.load(&[]).unwrap_err();
        assert!(
            error.0.starts_with("project.root is no longer supported"),
            "{error}"
        );
    }

    #[test]
    fn project_root_environment_is_relative_to_the_config() {
        let fixture = Fixture::new(CONFIG);
        fs::create_dir_all(fixture.root.join("fw/build")).unwrap();
        let config = fixture.load(&[("PROJECT_ROOT", "fw")]).unwrap();
        assert_eq!(config.project_dir, fixture.root.join("fw"));
        assert_eq!(config.elf, fixture.root.join("fw/build/demo.elf"));
        assert_eq!(config.run_dir, fixture.root.join(".tracebridge"));
        let error = fixture.load(&[("PROJECT_ROOT", "missing")]).unwrap_err();
        assert!(
            error
                .0
                .starts_with("PROJECT_ROOT does not resolve to a directory")
        );
    }

    #[test]
    fn practice_paths_are_not_resolved() {
        let fixture = Fixture::new(CONFIG);
        fixture.replace("script = \"flash.cmm\"", "script = \"~~/demo/flash.cmm\"");
        let config = fixture.load(&[]).unwrap();
        assert_eq!(config.resolved_flash_script(), "~~/demo/flash.cmm");
        assert!(config.flash_script_exists());
    }

    #[test]
    fn home_and_variables_are_expanded() {
        let fixture = Fixture::new(CONFIG);
        fixture.replace("sys = \"fake-t32\"", "sys = \"~/tools/$T32_VERSION\"");
        let config = fixture.load(&[("T32_VERSION", "2026")]).unwrap();
        assert_eq!(
            config.t32_sys,
            PathBuf::from("/nonexistent-tb-home/tools/2026")
        );
    }

    #[test]
    fn missing_and_invalid_files() {
        let error =
            load_config(Path::new("/nonexistent-tb/trace32.toml"), &Env::new()).unwrap_err();
        assert_eq!(error.0, "config not found: /nonexistent-tb/trace32.toml");
        let fixture = Fixture::new("[project\n");
        assert!(
            fixture
                .load(&[])
                .unwrap_err()
                .0
                .starts_with("cannot parse ")
        );
    }

    #[test]
    fn config_is_found_in_parent_directories() {
        let fixture = Fixture::new(CONFIG);
        let nested = fixture.root.join("src/deep");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            find_config_file(None, &nested, &Env::new()).unwrap(),
            fixture.file()
        );
        let error = find_config_file(None, Path::new("/"), &Env::new());
        // A trace32.toml at the filesystem root would be found; there is none.
        assert!(error.is_err());
    }

    #[test]
    fn explicit_config_wins_over_search() {
        let fixture = Fixture::new(CONFIG);
        let other = fixture.root.join("other");
        Fixture::write(&other, CONFIG);
        let found = find_config_file(
            Some(Path::new("other/trace32.toml")),
            &fixture.root,
            &Env::new(),
        )
        .unwrap();
        assert_eq!(found, other.join(CONFIG_FILE_NAME));
    }

    #[test]
    fn run_dir_gets_a_gitignore() {
        let fixture = Fixture::new(CONFIG);
        let config = fixture.load(&[]).unwrap();
        config.ensure_run_dir().unwrap();
        assert_eq!(
            fs::read_to_string(config.run_dir.join(".gitignore")).unwrap(),
            "*\n"
        );
    }
}
