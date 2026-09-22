//! Flash and load sequences (target.py). The command order is part of the
//! contract with the project's flash script and must not change.

use std::time::Duration;

use crate::config::Config;
use crate::errors::Result;
use crate::flash::{self, Choice};
use crate::powerview::require_file;
use crate::pycompat::Env;
use crate::remote::{CONNECT_TIMEOUT, Rcl, connect_debugger};
use crate::{bail, bridge_error};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Flash,
    Load,
}

impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Action::Flash => "flash",
            Action::Load => "load",
        }
    }
}

/// `validate_target_action`: the ELF must exist, and flashing needs a script
/// (returned, as passed to `DO`).
pub fn validate_target_action(
    config: &Config,
    action: Action,
    env: &Env,
) -> Result<Option<Choice>> {
    require_file(&config.elf, "ELF", false)?;
    if action == Action::Load {
        return Ok(None);
    }
    let choice = flash::resolve(config, env)?;
    if matches!(choice, Choice::Explicit(_)) && !config.flash_script_exists() {
        bail!("flash script not found: {}", config.resolved_flash_script());
    }
    Ok(Some(choice))
}

/// `run_target`. After connecting, every answer may take up to
/// `operation_timeout` seconds: `FLASH.ReProgram OFF` programs the flash and
/// can take much longer than the connection timeout.
pub fn run_target(config: &Config, script: Option<&str>) -> Result<()> {
    let action = if script.is_some() {
        Action::Flash
    } else {
        Action::Load
    };
    let mut debugger = connect_debugger(config, CONNECT_TIMEOUT)?;
    debugger
        .set_timeout(Duration::from_secs(config.operation_timeout))
        .map_err(|error| bridge_error!("TRACE32 {} failed: {error}", action.name()))?;
    run_sequence(config, script, &mut debugger)
}

/// Flash with `script` when given, then set up debugging.
pub fn run_sequence(config: &Config, script: Option<&str>, debugger: &mut impl Rcl) -> Result<()> {
    let action = if script.is_some() {
        Action::Flash
    } else {
        Action::Load
    };
    let result = (|| {
        if let Some(script) = script {
            program(config, script, debugger)?;
        }
        setup_debug(config, debugger)
    })();
    result.map_err(|error| {
        if error.is_timeout() {
            bridge_error!("TRACE32 operation exceeded {}s", config.operation_timeout)
        } else {
            bridge_error!("TRACE32 {} failed: {error}", action.name())
        }
    })
}

/// `program`: run the project's flash script in PREPAREONLY mode, then erase,
/// program and verify through FLASH.ReProgram.
pub fn program(config: &Config, script: &str, debugger: &mut impl Rcl) -> t32rcl::Result<()> {
    let mut command = format!("\"{script}\" PREPAREONLY");
    if !config.flash_args.is_empty() {
        command.push(' ');
        command.push_str(&config.flash_args.join(" "));
    }
    debugger.cmm(
        &command,
        Some(Duration::from_secs(config.operation_timeout)),
    )?;

    if !config.jtag_clock.is_empty() {
        debugger.cmd(&format!("SYStem.JtagClock {}", config.jtag_clock))?;
    }
    debugger.cmd("FLASH.ReProgram ALL /Erase")?;
    let loaded = debugger.cmd(&format!("Data.LOAD.Elf \"{}\"", config.elf.display()));
    // Always leave FLASH.ReProgram mode; a load failure is the primary error.
    let left = debugger.cmd("FLASH.ReProgram OFF");
    loaded?;
    left?;

    debugger.cmd("SYStem.Down")?;
    debugger.cmd("SYStem.Up")?;
    if !config.jtag_clock.is_empty() {
        debugger.cmd(&format!("SYStem.JtagClock {}", config.jtag_clock))?;
    }
    debugger.cmd("SYStem.Option.IMASKASM ON")?;
    debugger.cmd("SYStem.Option.IMASKHLL ON")?;
    debugger.print(&format!("tracebridge: flashed {}", config.elf.display()))
}

/// `setup_debug`: attach if needed, load symbols, enable RTOS awareness and run.
pub fn setup_debug(config: &Config, debugger: &mut impl Rcl) -> t32rcl::Result<()> {
    if !debugger.system_up()? {
        debugger.cmd("SYStem.Mode Down")?;
        if !config.cpu.is_empty() {
            debugger.cmd(&format!("SYStem.CPU {}", config.cpu))?;
        }
        if !config.mem_access.is_empty() {
            debugger.cmd(&format!("SYStem.MemAccess {}", config.mem_access))?;
        }
        if !config.cores.is_empty() {
            debugger.cmd(&format!("CORE.ASSIGN {}", config.cores))?;
        }
        debugger.cmd("SYStem.Mode Attach")?;
    }

    debugger.cmd(&format!(
        "Data.LOAD.Elf \"{}\" /NoCODE",
        config.elf.display()
    ))?;
    if !config.dual_port.is_empty() {
        debugger.cmd(&format!("SYStem.Option.DUALPORT {}", config.dual_port))?;
    }
    debugger.cmd("List.auto")?;

    let rtos_enabled = !config.rtos_config.is_empty() || !config.rtos_menu.is_empty();
    if !config.rtos_config.is_empty() {
        debugger.cmd(&format!("TASK.CONFIG \"{}\"", config.rtos_config))?;
    }
    if !config.rtos_menu.is_empty() {
        debugger.cmd(&format!("MENU.ReProgram \"{}\"", config.rtos_menu))?;
    }
    if rtos_enabled && config.rtos_show_tasks {
        debugger.cmd("TASK.List")?;
    }

    if !debugger.state_run()? {
        debugger.cmd("Go")?;
    }
    debugger.print(&format!(
        "tracebridge: symbols loaded for {}",
        config.program
    ))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::remote::tests::Recorder;
    use std::path::{Path, PathBuf};

    pub fn make_config(root: &Path) -> Config {
        let elf = root.join("demo.elf");
        std::fs::write(&elf, b"\x7fELF").unwrap();
        let flash = root.join("flash.cmm");
        std::fs::write(&flash, "").unwrap();
        Config {
            config_file: root.join("trace32.toml"),
            config_dir: root.to_path_buf(),
            run_dir: root.join(".tracebridge"),
            project_dir: root.to_path_buf(),
            program: "demo".into(),
            elf,
            cpu: "CPU".into(),
            cores: "1.".into(),
            mem_access: String::new(),
            jtag_clock: String::new(),
            dual_port: String::new(),
            rtos_config: String::new(),
            rtos_menu: String::new(),
            rtos_show_tasks: false,
            flash_chip: String::new(),
            flash_script: flash.to_string_lossy().into_owned(),
            flash_args: Vec::new(),
            t32_sys: root.to_path_buf(),
            t32_host: "test".into(),
            t32_binary: root.join("powerview"),
            t32_config: root.join("config.t32"),
            debug_adapter: root.join("adapter"),
            rcl_port: 20000,
            dap_port: 58870,
            dap_backend_port: 58871,
            dap_backend_timeout: 1,
            operation_timeout: 1,
            rtt_symbol: "_SEGGER_RTT".into(),
            rtt_control_block_address: None,
            rtt_poll_interval: 0.02,
        }
    }

    fn fixture() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let config = make_config(dir.path());
        (dir, config)
    }

    fn no_env() -> Env {
        [("XDG_CONFIG_HOME".to_string(), "/nonexistent-tb".to_string())].into()
    }

    // test_target.py: test_flash_requires_configured_script (now: a script or a chip)
    #[test]
    fn flash_requires_a_script_or_a_chip() {
        let (_dir, mut config) = fixture();
        config.flash_script = String::new();
        config.cpu = String::new();
        let error = validate_target_action(&config, Action::Flash, &no_env()).unwrap_err();
        assert!(
            error.0.starts_with("no flash script: set flash.chip"),
            "{error}"
        );
        config.cpu = "CPU".into();
        let error = validate_target_action(&config, Action::Flash, &no_env()).unwrap_err();
        assert!(
            error.0.starts_with("no flash script found for chip CPU"),
            "{error}"
        );
    }

    #[test]
    fn missing_elf_and_script_are_reported() {
        let (_dir, mut config) = fixture();
        config.flash_script = config.config_dir.join("nope.cmm").to_string_lossy().into();
        let error = validate_target_action(&config, Action::Flash, &no_env()).unwrap_err();
        assert!(error.0.starts_with("flash script not found: "));
        config.elf = PathBuf::from("/nonexistent-tb/app.elf");
        let error = validate_target_action(&config, Action::Load, &no_env()).unwrap_err();
        assert_eq!(error.0, "ELF not found: /nonexistent-tb/app.elf");
    }

    // test_target.py: test_load_is_driven_directly_through_rcl
    #[test]
    fn load_sequence_when_system_is_down() {
        let (_dir, mut config) = fixture();
        config.mem_access = "AXI".into();
        config.dual_port = "ON".into();
        config.rtos_config = "~~/freertos.t32".into();
        config.rtos_menu = "~~/freertos.men".into();
        config.rtos_show_tasks = true;
        let mut recorder = Recorder::default();
        run_sequence(&config, None, &mut recorder).unwrap();
        let elf = config.elf.display();
        assert_eq!(
            recorder.calls,
            [
                "system_up".to_string(),
                "cmd SYStem.Mode Down".into(),
                "cmd SYStem.CPU CPU".into(),
                "cmd SYStem.MemAccess AXI".into(),
                "cmd CORE.ASSIGN 1.".into(),
                "cmd SYStem.Mode Attach".into(),
                format!("cmd Data.LOAD.Elf \"{elf}\" /NoCODE"),
                "cmd SYStem.Option.DUALPORT ON".into(),
                "cmd List.auto".into(),
                "cmd TASK.CONFIG \"~~/freertos.t32\"".into(),
                "cmd MENU.ReProgram \"~~/freertos.men\"".into(),
                "cmd TASK.List".into(),
                "state_run".into(),
                "cmd Go".into(),
                "print tracebridge: symbols loaded for demo".into(),
            ]
        );
    }

    #[test]
    fn load_sequence_when_attached_and_running() {
        let (_dir, config) = fixture();
        let mut recorder = Recorder {
            system_up: true,
            state_run: true,
            ..Default::default()
        };
        run_sequence(&config, None, &mut recorder).unwrap();
        let elf = config.elf.display();
        assert_eq!(
            recorder.calls,
            [
                "system_up".to_string(),
                format!("cmd Data.LOAD.Elf \"{elf}\" /NoCODE"),
                "cmd List.auto".into(),
                "state_run".into(),
                "print tracebridge: symbols loaded for demo".into(),
            ]
        );
    }

    // test_target.py: test_flash_waits_for_project_cmm_then_programs
    #[test]
    fn flash_sequence() {
        let (_dir, mut config) = fixture();
        config.jtag_clock = "10MHz".into();
        config.flash_args = vec!["DUALPORT=1".into(), "X=2".into()];
        let mut recorder = Recorder {
            system_up: true,
            state_run: true,
            ..Default::default()
        };
        run_sequence(
            &config,
            Some(&config.resolved_flash_script()),
            &mut recorder,
        )
        .unwrap();
        let elf = config.elf.display();
        let script = config.resolved_flash_script();
        assert_eq!(
            recorder.calls,
            [
                format!("cmm \"{script}\" PREPAREONLY DUALPORT=1 X=2 Some(1s)"),
                "cmd SYStem.JtagClock 10MHz".into(),
                "cmd FLASH.ReProgram ALL /Erase".into(),
                format!("cmd Data.LOAD.Elf \"{elf}\""),
                "cmd FLASH.ReProgram OFF".into(),
                "cmd SYStem.Down".into(),
                "cmd SYStem.Up".into(),
                "cmd SYStem.JtagClock 10MHz".into(),
                "cmd SYStem.Option.IMASKASM ON".into(),
                "cmd SYStem.Option.IMASKHLL ON".into(),
                format!("print tracebridge: flashed {elf}"),
                "system_up".into(),
                format!("cmd Data.LOAD.Elf \"{elf}\" /NoCODE"),
                "cmd List.auto".into(),
                "state_run".into(),
                "print tracebridge: symbols loaded for demo".into(),
            ]
        );
    }

    // test_target.py: test_flash_leaves_reprogram_mode_when_loading_fails
    #[test]
    fn flash_leaves_reprogram_mode_when_loading_fails() {
        let (_dir, config) = fixture();
        let mut recorder = Recorder {
            fail_on: Some("Data.LOAD.Elf".into()),
            ..Default::default()
        };
        let error = program(&config, "flash.cmm", &mut recorder).unwrap_err();
        assert!(error.to_string().contains("Data.LOAD.Elf"));
        let commands = recorder.commands();
        let load = commands
            .iter()
            .position(|c| c.starts_with("Data.LOAD.Elf"))
            .unwrap();
        assert_eq!(commands[load + 1], "FLASH.ReProgram OFF");
        assert_eq!(commands.len(), load + 2);
    }

    #[test]
    fn failures_name_the_action() {
        let (_dir, config) = fixture();
        let mut recorder = Recorder {
            fail_on: Some("List.auto".into()),
            system_up: true,
            ..Default::default()
        };
        let error = run_sequence(&config, None, &mut recorder).unwrap_err();
        assert_eq!(error.0, "TRACE32 load failed: List.auto failed");
    }
}
