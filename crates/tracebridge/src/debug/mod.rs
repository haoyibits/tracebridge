//! `tracebridge debug`: inspect the target through the PowerView that is
//! already running, without ever resetting it.
//!
//! `tracebridge debug <command> [args]` runs one command; without a command it
//! opens an interactive session. Commands are read-only (R), change target or
//! debugger state (S), or only open a PowerView window (UI); R commands have no
//! side effects, so an allowlist of them is safe.

mod check;
mod commands;
mod decode;
mod elf;
mod perfile;
mod probe;
mod repl;
mod style;

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{CommandFactory, Parser, Subcommand};
use serde_json::json;
use t32rcl::Debugger;

use crate::config::Config;
use crate::errors::Result;
use crate::powerview;
use crate::remote::{CONNECT_TIMEOUT, connect_debugger};
use commands::Context;
use probe::{DResult, DebugError, PerSnapshot, Probe};
use style::Style;

/// Exit code when a check fails or `verify` finds a mismatch (1 means the
/// command itself could not run, 2 is a usage error).
pub const EXIT_FAILED: i32 = 3;

const AFTER_HELP: &str = "\
Kinds: [R] reads only, changes nothing; [S] changes target or debugger state;
[UI] opens a PowerView window. 'check' is R without --halt and S with it.

Without a command, an interactive session starts. Exit codes: 0 ok, 1 error,
2 usage, 3 a check failed or verify found a difference.";

#[derive(Debug, Parser)]
#[command(
    name = "tracebridge debug",
    no_binary_name = true,
    disable_help_subcommand = true,
    disable_version_flag = true,
    about = "Inspect the target through the running PowerView; never resets it",
    after_help = AFTER_HELP
)]
pub struct DebugCli {
    /// Machine-readable JSON output (one JSON document per command)
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<DebugCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum DebugCommand {
    /// [R] Debugger mode, run state, power, CPU; PC and CPSR when halted
    Status,
    /// [R] Registers by PER name (HSR, HSCTLR.C), full PER path, or raw address (C15:0x4025)
    Reg {
        #[arg(required = true, value_name = "NAME|ADDRESS")]
        names: Vec<String>,
    },
    /// [R] 32-bit words at an address (AD:0x20000000) or a symbol
    Mem {
        #[arg(value_name = "ADDRESS|SYMBOL")]
        location: String,
        /// Number of 32-bit words
        #[arg(default_value_t = 1)]
        count: u32,
    },
    /// [R] AArch32 Hyp fault report: vector slot, HSR decoded, HDFAR/HIFAR, ELR_hyp, SPSR_hyp
    Fault,
    /// [R] Print the value of a PRACTICE expression
    Eval {
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        expression: Vec<String>,
    },
    /// [R] Compare target memory with the ELF's loadable content at the load addresses
    Verify {
        /// ELF file (default: project.elf)
        elf: Option<PathBuf>,
        /// Also run TRACE32's own comparison (Data.LOAD.Elf /DIFF)
        #[arg(long)]
        t32: bool,
    },
    /// [R] Run a check file; [S] with --halt, which may stop the core
    Check {
        file: PathBuf,
        /// Run the checks listed for this variant
        #[arg(long)]
        variant: Option<String>,
        /// Stop the core (Break) when checks read CP15 or core registers
        #[arg(long)]
        halt: bool,
        /// Only resolve register names and symbols; read nothing
        #[arg(long, conflicts_with = "halt")]
        dry_run: bool,
    },
    /// [UI] Open a PER.Watch window with these registers (names, or a file with one per line)
    Watch {
        #[arg(required = true, value_name = "FILE|NAME")]
        items: Vec<String>,
    },
    /// [S] SYStem.Mode Attach: no reset, the core keeps running or stays halted
    Attach,
    /// [S] SYStem.Down
    Down,
    /// [S] Halt the core
    Break,
    /// [S] Resume the core
    Go,
    /// [S] Execute any PRACTICE command (can do anything, including reset and flash)
    Cmd {
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Connect again after the RCL connection was lost
    Reconnect,
    /// Help for the session or one command
    Help { command: Option<String> },
    /// Leave the session
    #[command(alias = "exit")]
    Quit,
}

impl DebugCommand {
    pub fn name(&self) -> &'static str {
        match self {
            DebugCommand::Status => "status",
            DebugCommand::Reg { .. } => "reg",
            DebugCommand::Mem { .. } => "mem",
            DebugCommand::Fault => "fault",
            DebugCommand::Eval { .. } => "eval",
            DebugCommand::Verify { .. } => "verify",
            DebugCommand::Check { .. } => "check",
            DebugCommand::Watch { .. } => "watch",
            DebugCommand::Attach => "attach",
            DebugCommand::Down => "down",
            DebugCommand::Break => "break",
            DebugCommand::Go => "go",
            DebugCommand::Cmd { .. } => "cmd",
            DebugCommand::Reconnect => "reconnect",
            DebugCommand::Help { .. } => "help",
            DebugCommand::Quit => "quit",
        }
    }
}

/// The result of a command: human text, the JSON document and the exit code.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub text: String,
    pub json: serde_json::Value,
    pub code: i32,
}

impl Outcome {
    pub fn ok(text: String, json: serde_json::Value) -> Outcome {
        Outcome {
            text,
            json,
            code: 0,
        }
    }
}

/// `eval` and `cmd` take the rest of the line as PRACTICE text, so a trailing
/// `--json` (e.g. `debug eval Register(PC) --json`) is taken off first.
pub fn split_trailing_json(mut args: Vec<String>) -> (Vec<String>, bool) {
    let takes_rest = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .is_some_and(|name| name == "eval" || name == "cmd");
    let mut json = false;
    if takes_rest {
        while args.len() > 2 && args.last().is_some_and(|arg| arg == "--json") {
            args.pop();
            json = true;
        }
    }
    (args, json)
}

/// Parse command-line style arguments.
pub fn parse(args: Vec<String>) -> std::result::Result<(DebugCli, bool), clap::Error> {
    let (args, trailing_json) = split_trailing_json(args);
    let cli = DebugCli::try_parse_from(args)?;
    let json = cli.json || trailing_json;
    Ok((cli, json))
}

/// Help for the session, or for one command.
pub fn help_text(command: Option<&str>, style: Style) -> std::result::Result<String, String> {
    let mut cli = DebugCli::command();
    let help = match command {
        None => cli.render_help(),
        Some(name) => match cli.find_subcommand_mut(name) {
            Some(sub) => sub.render_help(),
            None => return Err(format!("unknown command '{name}'; 'help' lists them")),
        },
    };
    Ok(if style.enabled() {
        help.ansi().to_string()
    } else {
        help.to_string()
    })
}

/// Run one command (everything except the session commands).
pub fn execute(ctx: &mut Context, command: &DebugCommand) -> DResult<Outcome> {
    match command {
        DebugCommand::Status => commands::status(ctx),
        DebugCommand::Reg { names } => commands::reg(ctx, names),
        DebugCommand::Mem { location, count } => commands::mem(ctx, location, *count),
        DebugCommand::Fault => commands::fault(ctx),
        DebugCommand::Eval { expression } => commands::eval(ctx, &expression.join(" ")),
        DebugCommand::Verify { elf, t32 } => commands::verify(ctx, elf.as_deref(), *t32),
        DebugCommand::Check {
            file,
            variant,
            halt,
            dry_run,
        } => commands::check(
            ctx,
            file,
            &check::Options {
                variant: variant.clone(),
                halt: *halt,
                dry_run: *dry_run,
            },
        ),
        DebugCommand::Watch { items } => commands::watch(ctx, items),
        DebugCommand::Attach => commands::attach(ctx),
        DebugCommand::Down => commands::command(ctx, "SYStem.Down", "SYStem.Down done"),
        DebugCommand::Break => commands::command(ctx, "Break", "halted"),
        DebugCommand::Go => commands::command(ctx, "Go", "running"),
        DebugCommand::Cmd { command } => {
            commands::command(ctx, &command.join(" "), "command executed")
        }
        DebugCommand::Reconnect | DebugCommand::Help { .. } | DebugCommand::Quit => Err(
            DebugError::new(format!("'{}' is a session command", command.name())),
        ),
    }
}

/// Print a command's result and return its exit code.
pub fn emit(command: &str, json: bool, result: DResult<Outcome>) -> i32 {
    match result {
        Ok(outcome) => {
            if json {
                println!(
                    "{}",
                    json!({
                        "command": command,
                        "ok": outcome.code == 0,
                        "exit_code": outcome.code,
                        "result": outcome.json,
                    })
                );
            } else if !outcome.text.is_empty() {
                println!("{}", outcome.text);
            }
            outcome.code
        }
        Err(error) => {
            if json {
                println!(
                    "{}",
                    json!({
                        "command": command,
                        "ok": false,
                        "exit_code": 1,
                        "error": error.message,
                        "connection_lost": error.lost,
                    })
                );
            }
            style::error(&error);
            1
        }
    }
}

/// Connect to the PowerView that is already running. Nothing here starts
/// PowerView or touches the target: the RCL attach and version check only.
pub fn connect(config: &Config) -> DResult<Debugger> {
    if !powerview::port_open(config.rcl_port) {
        return Err(DebugError::new(format!(
            "no PowerView on RCL port {}; start it with 'tracebridge open' \
             ('debug' never starts PowerView or resets the target)",
            config.rcl_port
        )));
    }
    let mut debugger =
        connect_debugger(config, CONNECT_TIMEOUT).map_err(|error| DebugError::new(error.0))?;
    // verify and cmd may take long; the other answers are quick.
    debugger.set_timeout(Duration::from_secs(config.operation_timeout))?;
    Ok(debugger)
}

/// Handle what needs no configuration or PowerView: usage errors, `--help`,
/// `help` and `quit`. Returns the exit code, or `None` to go on.
pub fn main_without_config(args: &[String]) -> Option<i32> {
    let cli = match parse(args.to_vec()) {
        Ok((cli, _)) => cli,
        Err(error) => {
            let _ = error.print();
            return Some(error.exit_code());
        }
    };
    match cli.command {
        Some(DebugCommand::Help { command }) => {
            Some(match help_text(command.as_deref(), Style::stdout()) {
                Ok(text) => {
                    print!("{text}");
                    0
                }
                Err(message) => {
                    style::error(message);
                    2
                }
            })
        }
        Some(DebugCommand::Quit) => Some(0),
        _ => None,
    }
}

/// `tracebridge debug [args]`.
pub fn main(config: &Config, cwd: &Path, args: Vec<String>) -> Result<i32> {
    if let Some(code) = main_without_config(&args) {
        return Ok(code);
    }
    let (cli, json) = parse(args).expect("checked by main_without_config");
    let Some(command) = cli.command else {
        return repl::run(config, cwd, json);
    };
    let result = connect(config).and_then(|mut debugger| {
        if command == DebugCommand::Reconnect {
            return Ok(Outcome::ok(
                format!("connected to PowerView on RCL port {}", config.rcl_port),
                json!({"rcl_port": config.rcl_port}),
            ));
        }
        let mut per = PerSnapshot::new(Some(config.t32_sys.clone()));
        let mut ctx = Context {
            probe: &mut debugger as &mut dyn Probe,
            per: &mut per,
            config,
            cwd,
            style: Style::stdout(),
        };
        execute(&mut ctx, &command)
    });
    Ok(emit(command.name(), json, result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_commands() {
        let (cli, json) = parse(args("reg HSR HSCTLR.C --json")).unwrap();
        assert!(json);
        assert_eq!(
            cli.command,
            Some(DebugCommand::Reg {
                names: vec!["HSR".into(), "HSCTLR.C".into()]
            })
        );
        let (cli, _) = parse(args("mem AD:0x100")).unwrap();
        assert_eq!(
            cli.command,
            Some(DebugCommand::Mem {
                location: "AD:0x100".into(),
                count: 1
            })
        );
        let (cli, _) = parse(args("check c.toml --variant a --halt")).unwrap();
        assert_eq!(
            cli.command,
            Some(DebugCommand::Check {
                file: "c.toml".into(),
                variant: Some("a".into()),
                halt: true,
                dry_run: false
            })
        );
        assert!(parse(args("check c.toml --halt --dry-run")).is_err());
        assert!(parse(args("reg")).is_err());
        assert!(parse(args("up")).is_err());
        let (cli, json) = parse(Vec::new()).unwrap();
        assert_eq!(cli.command, None);
        assert!(!json);
        let (cli, _) = parse(args("exit")).unwrap();
        assert_eq!(cli.command, Some(DebugCommand::Quit));
    }

    #[test]
    fn eval_and_cmd_keep_their_text_but_not_a_trailing_json() {
        let (cli, json) = parse(args("eval Data.Long(AD:0x0)+-1 --json")).unwrap();
        assert!(json);
        assert_eq!(
            cli.command,
            Some(DebugCommand::Eval {
                expression: vec!["Data.Long(AD:0x0)+-1".into()]
            })
        );
        let (cli, json) = parse(args("--json cmd PRINT -1")).unwrap();
        assert!(json);
        assert_eq!(
            cli.command,
            Some(DebugCommand::Cmd {
                command: vec!["PRINT".into(), "-1".into()]
            })
        );
        // A lone "--json" is the expression, not a flag.
        let (args, json) = split_trailing_json(args("eval --json"));
        assert_eq!(args, ["eval", "--json"]);
        assert!(!json);
    }

    #[test]
    fn session_commands_are_not_executed_as_target_commands() {
        let mut probe = probe::fake::FakeProbe::default();
        let dir = tempfile::tempdir().unwrap();
        let config = crate::target::tests::make_config(dir.path());
        let mut per = PerSnapshot::default();
        let mut ctx = Context {
            probe: &mut probe,
            per: &mut per,
            config: &config,
            cwd: dir.path(),
            style: Style::PLAIN,
        };
        let error = execute(&mut ctx, &DebugCommand::Quit).unwrap_err();
        assert_eq!(error.message, "'quit' is a session command");
    }

    #[test]
    fn help_lists_kinds() {
        let text = help_text(None, Style::PLAIN).unwrap();
        assert!(text.contains("[R] Debugger mode"), "{text}");
        assert!(text.contains("[S] SYStem.Down"));
        assert!(text.contains("[UI] Open a PER.Watch window"));
        assert!(
            help_text(Some("reg"), Style::PLAIN)
                .unwrap()
                .contains("NAME|ADDRESS")
        );
        assert!(help_text(Some("nope"), Style::PLAIN).is_err());
        // The same text, with clap's styling.
        let colored = help_text(None, Style::COLOR).unwrap();
        assert!(colored.contains("\x1b["), "{colored}");
        assert_eq!(style::strip(&colored), text);
    }
}
