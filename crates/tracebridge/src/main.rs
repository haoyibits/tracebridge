//! tracebridge: Lauterbach TRACE32 PowerView from the command line, VS Code and
//! RustRover (cli.py and t32.py of the Python tool).

mod config;
mod dap;
mod errors;
mod init;
mod powerview;
mod pycompat;
mod remote;
mod rtt;
mod rustrover;
mod t32config;
mod target;
mod ui;
mod vscode;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::config::{Config, find_config_file, load_config};
use crate::errors::Result;
use crate::target::Action;
use crate::ui::info;

const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("TRACEBRIDGE_GIT_HASH"),
    ")"
);

#[derive(Debug, Parser)]
#[command(
    name = "tracebridge",
    version = VERSION,
    about = "Drive Lauterbach TRACE32 PowerView from the command line, VS Code and RustRover",
    after_help = "Configuration: the nearest trace32.toml in the current directory or a parent \
                  (create one with 'tracebridge init')."
)]
struct Cli {
    /// Configuration file (default: the nearest trace32.toml upwards from the current directory)
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a commented trace32.toml in the current directory
    Init,
    /// Print the resolved configuration and check paths
    Config,
    /// Start PowerView, or reuse a running one
    Open,
    /// Flash the ELF, load symbols and run
    Flash,
    /// Load symbols without programming, then run
    Load,
    /// Interactive SEGGER RTT terminal (see 'tracebridge rtt --help')
    #[command(disable_help_flag = true)]
    Rtt {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
    },
    /// Run the DAP proxy in front of t32debugadapter (started by the IDE)
    Adapter,
    /// Write .vscode/launch.json and tasks.json for debugging with F5
    Vscode,
    /// Write a RustRover run configuration (needs the LSP4IJ plugin)
    Rustrover,
}

fn main() {
    install_interrupt_handler();
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("tracebridge: {error}");
            1
        }
    };
    std::process::exit(code);
}

/// Ctrl-C ends the process with exit code 130 (the Python tool's
/// `SystemExit(130)`). The RTT terminal and the DAP proxy install their own
/// handlers because they need to clean up.
fn install_interrupt_handler() {
    #[cfg(unix)]
    {
        use nix::sys::signal::{SigHandler, Signal, signal};
        extern "C" fn on_interrupt(_: nix::libc::c_int) {
            // SAFETY: _exit is async-signal-safe.
            unsafe { nix::libc::_exit(130) };
        }
        // SAFETY: the handler only calls an async-signal-safe function.
        unsafe {
            let _ = signal(Signal::SIGINT, SigHandler::Handler(on_interrupt));
        }
    }
}

fn run(cli: Cli) -> Result<i32> {
    let cwd = std::env::current_dir()
        .map_err(|error| bridge_error!("cannot determine the current directory: {error}"))?;
    if let Command::Init = cli.command {
        let path = init::init(&cwd)?;
        info(&format!("created {}", path.display()));
        println!(
            "\nNext steps:\n  1. Edit program, elf, [target] and flash.script in trace32.toml\n  \
             2. tracebridge config    # every path should be ok\n  \
             3. tracebridge flash     # or: tracebridge load\n  \
             4. tracebridge vscode    # or: tracebridge rustrover, to debug from the IDE"
        );
        return Ok(0);
    }

    let env = pycompat::process_env();
    let config_file = find_config_file(cli.config.as_deref(), &cwd, &env)?;
    let config = load_config(&config_file, &env)?;
    match cli.command {
        Command::Init => unreachable!(),
        Command::Config => print_config(&config),
        Command::Open => open(&config, None)?,
        Command::Flash => open(&config, Some(Action::Flash))?,
        Command::Load => open(&config, Some(Action::Load))?,
        Command::Adapter => adapter(&config)?,
        Command::Rtt { args } => {
            let args = rtt::parse_args(&args);
            if !powerview::port_open(config.rcl_port) {
                bail!(
                    "no PowerView on RCL port {}; run 'tracebridge open', 'flash' or 'load' first",
                    config.rcl_port
                );
            }
            rtt::run(&config, args)?
        }
        Command::Vscode => vscode::installer::install(&config, &ui::current_exe()?)?,
        Command::Rustrover => {
            rustrover::install(&config, &ui::current_exe()?)?;
        }
    }
    Ok(0)
}

/// `adapter` (`_run` in cli.py): the IDE starts it before attaching.
fn adapter(config: &Config) -> Result<()> {
    if powerview::port_open(config.dap_port) {
        info(&format!(
            "debug adapter already listening on {}",
            config.dap_port
        ));
        return Ok(());
    }
    if !powerview::port_open(config.rcl_port) {
        bail!(
            "no PowerView on RCL port {}; run 'tracebridge open', 'flash' or 'load' first",
            config.rcl_port
        );
    }
    info(&format!(
        "starting debug adapter proxy on port {}",
        config.dap_port
    ));
    dap::proxy::run_proxy(config)
}

/// `open`, `flash` and `load` (`_run` in cli.py).
fn open(config: &Config, action: Option<Action>) -> Result<()> {
    if powerview::start_powerview(config)? {
        info("PowerView ready");
    } else {
        info(&format!(
            "reusing PowerView on RCL port {}",
            config.rcl_port
        ));
    }
    let Some(action) = action else {
        return Ok(());
    };
    let verb = match action {
        Action::Flash => "flashing",
        Action::Load => "loading symbols from",
    };
    info(&format!("{verb} {}", config.elf.display()));
    target::run_target(config, action)?;
    info(match action {
        Action::Flash => "flashed, symbols loaded, target running",
        Action::Load => "symbols loaded, target running",
    });
    Ok(())
}

fn status(path: &Path) -> &'static str {
    if path.exists() { "ok     " } else { "MISSING" }
}

/// `_print_config`, plus the flash script, ports and the Remote API check.
fn print_config(config: &Config) {
    info(&format!("configuration: {}", config.config_file.display()));
    let entries: [(&str, &Path); 5] = [
        ("project", &config.project_dir),
        ("ELF", &config.elf),
        ("T32_BIN", &config.t32_binary),
        ("T32_CONFIG", &config.t32_config),
        ("T32_DEBUG_ADAPTER", &config.debug_adapter),
    ];
    for (name, path) in entries {
        println!("  {} {name}={}", status(path), path.display());
    }
    let script = config.resolved_flash_script();
    if script.is_empty() {
        println!("  -       flash.script is empty (use load for RAM images)");
    } else {
        let state = if config.flash_script_exists() {
            "ok     "
        } else {
            "MISSING"
        };
        println!("  {state} flash.script={script}");
    }
    println!(
        "  ports   RCL {}, DAP {} (t32debugadapter {})",
        config.rcl_port, config.dap_port, config.dap_backend_port
    );
    println!("  run     {}", config.run_dir.display());
    if let Ok(exe) = ui::current_exe() {
        println!("  exe     {}", exe.display());
    }
    if config.t32_config.is_file() {
        match t32config::rcl_settings(&config.t32_config) {
            None => println!(
                "  note    config.t32 does not enable the Remote API; tracebridge adds \
                 --t32-api-rcl=TCP:{} when it starts PowerView",
                config.rcl_port
            ),
            Some(settings) if !settings.protocol.eq_ignore_ascii_case("NETTCP") => println!(
                "  WARN    config.t32 has RCL={}; tracebridge needs RCL=NETTCP",
                settings.protocol
            ),
            Some(settings) if settings.port != Some(config.rcl_port) => println!(
                "  WARN    config.t32 PORT={} differs from trace32.rcl_port {}",
                settings
                    .port
                    .map(|port| port.to_string())
                    .unwrap_or_else(|| "(none)".into()),
                config.rcl_port
            ),
            Some(_) => {}
        }
    }
}
