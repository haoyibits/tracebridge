//! tracebridge: Lauterbach TRACE32 PowerView from the command line, VS Code and
//! RustRover (cli.py and t32.py of the Python tool).
//!
//! `unsafe` is denied crate-wide. Code that needs an operating-system interface
//! without a safe API opts in with `#[allow(unsafe_code)]` on the enclosing
//! function and explains itself in a `// SAFETY:` comment; today that is only
//! the `setsid` call in `powerview::spawn_powerview`.

#![deny(unsafe_code)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod config;
mod dap;
mod errors;
mod flash;
mod init;
mod powerview;
mod pycompat;
mod remote;
mod rtt;
mod rustrover;
mod signals;
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
    Flash {
        /// Choose the flash script for this chip (overrides flash.chip and target.cpu)
        #[arg(long, conflicts_with = "script")]
        chip: Option<String>,
        /// Use this flash script (overrides flash.script; "~~/..." is a TRACE32 path)
        #[arg(long, value_name = "PATH")]
        script: Option<String>,
    },
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
    /// Find the flash scripts for a chip (your library and the TRACE32 installation)
    Chips {
        /// Chip name or part of it, e.g. SR6P6 or STM32H743ZI (default: flash.chip or target.cpu)
        query: Option<String>,
    },
}

fn main() {
    // Ctrl-C exits with 130 and a closed stdout ends quietly; the long-running
    // commands handle both themselves (see signals.rs).
    let handlers = signals::Handlers::install();
    let cli = Cli::parse();
    if matches!(cli.command, Command::Rtt { .. } | Command::Adapter) {
        handlers.release();
    }
    let code = match run(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("tracebridge: {error}");
            1
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<i32> {
    let cwd = std::env::current_dir()
        .map_err(|error| bridge_error!("cannot determine the current directory: {error}"))?;
    if let Command::Init = cli.command {
        let path = init::init(&cwd)?;
        info(&format!("created {}", path.display()));
        println!(
            "\nNext steps:\n  1. Edit program, elf, [target] and [flash] in trace32.toml\n  \
             2. tracebridge config    # every path should be ok\n  \
             3. tracebridge flash     # or: tracebridge load\n  \
             4. tracebridge vscode    # or: tracebridge rustrover, to debug from the IDE"
        );
        return Ok(0);
    }

    let env = pycompat::process_env();
    let config_file = find_config_file(cli.config.as_deref(), &cwd, &env)?;
    let mut config = load_config(&config_file, &env)?;
    match cli.command {
        Command::Init => unreachable!(),
        Command::Config => print_config(&config, &env),
        Command::Open => open(&config, None, &env)?,
        Command::Flash { chip, script } => {
            if let Some(chip) = chip {
                config.flash_script.clear();
                config.flash_chip = chip;
            }
            if let Some(script) = script {
                config.flash_script = script;
            }
            open(&config, Some(Action::Flash), &env)?
        }
        Command::Load => open(&config, Some(Action::Load), &env)?,
        Command::Chips { query } => chips(&config, query.as_deref(), &env)?,
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
fn open(config: &Config, action: Option<Action>, env: &pycompat::Env) -> Result<()> {
    // Check the ELF and the flash script before starting PowerView.
    let choice = match action {
        Some(action) => target::validate_target_action(config, action, env)?,
        None => None,
    };
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
    let mut config = config.clone();
    if let Some(choice) = &choice {
        info(&format!("flash script {}", describe(choice)));
        // Family scripts need the derivative; without CPU= they use a default one.
        if let Some(argument) = choice.cpu_argument(&config.flash_args) {
            info(&format!("passing {argument} to the flash script"));
            config.flash_args.push(argument);
        }
    }
    let config = &config;
    info(&format!("{verb} {}", config.elf.display()));
    target::run_target(
        config,
        choice.as_ref().map(flash::Choice::script).as_deref(),
    )?;
    info(match action {
        Action::Flash => "flashed, symbols loaded, target running",
        Action::Load => "symbols loaded, target running",
    });
    Ok(())
}

fn describe(choice: &flash::Choice) -> String {
    match choice {
        flash::Choice::Explicit(script) => format!("{script} (flash.script)"),
        flash::Choice::Chip {
            chip,
            pattern,
            script,
        } => format!(
            "{} ({} script for {chip}, @Chip {pattern})",
            script.path.display(),
            script.source.name()
        ),
    }
}

/// `tracebridge chips`: which script `flash` would use, and related scripts.
fn chips(config: &Config, query: Option<&str>, env: &pycompat::Env) -> Result<()> {
    let Some(query) = query.or_else(|| flash::chip_name(config)) else {
        bail!("give a chip name, e.g. 'tracebridge chips STM32H743ZI'");
    };
    let scripts = flash::catalog(config, env);
    let library = flash::library_dir(env);
    let chosen = flash::choose(query, &scripts);
    match &chosen {
        Ok((pattern, script)) => info(&format!(
            "{query}: {} ({} script, @Chip {pattern})",
            script.path.display(),
            script.source.name()
        )),
        Err(error) => info(&format!("{query}: {error}")),
    }
    let related = flash::search(query, &scripts);
    if !related.is_empty() {
        println!("  related scripts:");
    }
    for script in related.iter().take(40) {
        let mark = match &chosen {
            Ok((_, chosen)) if chosen.path == script.path => "*",
            _ => " ",
        };
        let prepare = if script.prepare_only {
            ""
        } else {
            "  (no PREPAREONLY)"
        };
        println!(
            "  {mark} {:<8} {:<28} {}{prepare}",
            script.source.name(),
            script.chips.join(" "),
            script.path.display()
        );
    }
    if related.len() > 40 {
        println!("  ... {} more; narrow the query", related.len() - 40);
    }
    println!(
        "
  library: {} ({} scripts); TRACE32: {}",
        library.display(),
        scripts
            .iter()
            .filter(|s| s.source == flash::Source::Library)
            .count(),
        config.t32_sys.join("demo/*/flash").display()
    );
    println!(
        "  Use it with flash.chip = \"{query}\" in trace32.toml or 'tracebridge flash --chip {query}'."
    );
    Ok(())
}

fn status(path: &Path) -> &'static str {
    if path.exists() { "ok     " } else { "MISSING" }
}

/// `_print_config`, plus the flash script, ports and the Remote API check.
fn print_config(config: &Config, env: &pycompat::Env) {
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
    match flash::resolve(config, env) {
        Ok(choice @ flash::Choice::Explicit(_)) => {
            let state = if config.flash_script_exists() {
                "ok     "
            } else {
                "MISSING"
            };
            println!("  {state} flash script={}", describe(&choice));
        }
        Ok(choice) => println!("  ok      flash script={}", describe(&choice)),
        Err(error) => println!("  MISSING flash script: {error}"),
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
