//! The interactive `tracebridge debug` session: line editing, history in
//! `<project>/.tracebridge/debug_history`, command-name completion, and a
//! prompt that shows the debugger state (`t32 [up, halted]>`).
//!
//! A failing command never ends the session. A lost RCL connection is
//! reported and `reconnect` retries it.

use std::path::{Path, PathBuf};

use rustyline::completion::Completer;
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Editor, Helper};
use serde_json::json;

use super::commands::Context;
use super::probe::{DResult, DebuggerState, PerSnapshot, Probe};
use super::{DebugCli, DebugCommand, Outcome, connect, emit, execute, help_text, parse};
use crate::bridge_error;
use crate::config::Config;
use crate::errors::Result;
use crate::pycompat::shlex_split;
use crate::ui::info;

/// Split a session line like a shell command line, except that `eval` and
/// `cmd` take the rest of the line verbatim (PRACTICE text keeps its quotes).
pub fn split_line(line: &str) -> std::result::Result<Vec<String>, String> {
    let trimmed = line.trim();
    let (first, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, rest.trim()),
        None => (trimmed, ""),
    };
    if first == "eval" || first == "cmd" {
        let mut rest = rest.to_string();
        let mut json = false;
        while let Some(stripped) = rest.strip_suffix("--json") {
            if !stripped.ends_with(char::is_whitespace) {
                break;
            }
            rest = stripped.trim_end().to_string();
            json = true;
        }
        let mut args = vec![first.to_string()];
        if !rest.is_empty() {
            args.push(rest);
        }
        if json {
            args.push("--json".into());
        }
        return Ok(args);
    }
    shlex_split(trimmed).map_err(str::to_string)
}

/// The command names, for completion.
pub fn command_names() -> Vec<String> {
    use clap::CommandFactory;
    DebugCli::command()
        .get_subcommands()
        .flat_map(|sub| {
            std::iter::once(sub.get_name().to_string())
                .chain(sub.get_all_aliases().map(str::to_string))
        })
        .collect()
}

/// Complete the command name (also after `help`).
pub fn complete(names: &[String], line: &str, pos: usize) -> (usize, Vec<String>) {
    let before = &line[..pos];
    let (start, word) = match before.rsplit_once(char::is_whitespace) {
        Some((head, word)) if head.trim() == "help" => (pos - word.len(), word),
        Some(_) => return (pos, Vec::new()),
        None => (
            before.len() - before.trim_start().len(),
            before.trim_start(),
        ),
    };
    let mut matches: Vec<String> = names
        .iter()
        .filter(|name| name.starts_with(word))
        .cloned()
        .collect();
    matches.sort();
    (start, matches)
}

struct CommandHelper {
    names: Vec<String>,
}

impl Completer for CommandHelper {
    type Candidate = String;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<String>)> {
        Ok(complete(&self.names, line, pos))
    }
}

impl Hinter for CommandHelper {
    type Hint = String;
}

impl Highlighter for CommandHelper {}

impl Validator for CommandHelper {}

impl Helper for CommandHelper {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

/// Session state, independent of the terminal so that tests can drive it.
pub struct Session<'a, P> {
    config: &'a Config,
    cwd: &'a Path,
    json: bool,
    connector: Box<dyn FnMut() -> DResult<P> + 'a>,
    probe: Option<P>,
    per: PerSnapshot,
}

impl<'a, P: Probe> Session<'a, P> {
    pub fn new(
        config: &'a Config,
        cwd: &'a Path,
        json: bool,
        connector: Box<dyn FnMut() -> DResult<P> + 'a>,
        probe: P,
    ) -> Self {
        Session {
            config,
            cwd,
            json,
            connector,
            probe: Some(probe),
            per: PerSnapshot::new(Some(config.t32_sys.clone())),
        }
    }

    #[cfg(test)]
    pub fn connected(&self) -> bool {
        self.probe.is_some()
    }

    fn lost(&mut self, message: &str) {
        self.probe = None;
        eprintln!("tracebridge: RCL connection lost ({message}); 'reconnect' retries");
    }

    /// `t32 [up, halted]> `, `t32 [down]> `, `t32 [disconnected]> `.
    pub fn prompt(&mut self) -> String {
        let Some(probe) = self.probe.as_mut() else {
            return "t32 [disconnected]> ".into();
        };
        match DebuggerState::read(probe) {
            Ok(state) => format!("t32 [{}]> ", state.label()),
            Err(error) if error.lost => {
                self.lost(&error.message);
                "t32 [disconnected]> ".into()
            }
            Err(_) => "t32 [?]> ".into(),
        }
    }

    pub fn handle_line(&mut self, line: &str) -> Flow {
        let args = match split_line(line) {
            Ok(args) => args,
            Err(error) => {
                eprintln!("tracebridge: {error}");
                return Flow::Continue;
            }
        };
        if args.is_empty() {
            return Flow::Continue;
        }
        let (cli, json) = match parse(args) {
            Ok(parsed) => parsed,
            Err(error) => {
                let _ = error.print();
                return Flow::Continue;
            }
        };
        let json = json || self.json;
        let Some(command) = cli.command else {
            return Flow::Continue;
        };
        match &command {
            DebugCommand::Quit => return Flow::Quit,
            DebugCommand::Help { command } => {
                match help_text(command.as_deref()) {
                    Ok(text) => print!("{text}"),
                    Err(message) => eprintln!("tracebridge: {message}"),
                }
                return Flow::Continue;
            }
            DebugCommand::Reconnect => {
                self.probe = None;
                let result = (self.connector)().map(|probe| {
                    self.probe = Some(probe);
                    self.per.invalidate();
                    Outcome::ok(
                        format!(
                            "connected to PowerView on RCL port {}",
                            self.config.rcl_port
                        ),
                        json!({"rcl_port": self.config.rcl_port}),
                    )
                });
                emit("reconnect", json, result);
                return Flow::Continue;
            }
            _ => {}
        }
        let Some(probe) = self.probe.as_mut() else {
            eprintln!("tracebridge: not connected to PowerView; 'reconnect' retries");
            return Flow::Continue;
        };
        let mut ctx = Context {
            probe,
            per: &mut self.per,
            config: self.config,
            cwd: self.cwd,
        };
        let result = execute(&mut ctx, &command);
        let lost = match &result {
            Err(error) if error.lost => Some(error.message.clone()),
            _ => None,
        };
        emit(command.name(), json, result);
        if let Some(message) = lost {
            self.lost(&message);
        }
        Flow::Continue
    }
}

fn history_path(config: &Config) -> Option<PathBuf> {
    config
        .ensure_run_dir()
        .ok()
        .map(|()| config.run_dir.join("debug_history"))
}

/// The interactive session.
pub fn run(config: &Config, cwd: &Path, json: bool) -> Result<i32> {
    let debugger = connect(config).map_err(|error| bridge_error!("{error}"))?;
    let mut session = Session::new(config, cwd, json, Box::new(|| connect(config)), debugger);
    let mut editor = Editor::<CommandHelper, DefaultHistory>::new()
        .map_err(|error| bridge_error!("cannot start the line editor: {error}"))?;
    editor.set_helper(Some(CommandHelper {
        names: command_names(),
    }));
    let history = history_path(config);
    if let Some(path) = &history {
        let _ = editor.load_history(path);
    }
    if !json {
        info(&format!(
            "connected to PowerView on RCL port {}; 'help' lists the commands, 'quit' leaves",
            config.rcl_port
        ));
    }
    loop {
        let prompt = session.prompt();
        match editor.readline(&prompt) {
            Ok(line) => {
                if !line.trim().is_empty() {
                    let _ = editor.add_history_entry(line.as_str());
                    if let Some(path) = &history {
                        let _ = editor.append_history(path);
                    }
                }
                if session.handle_line(&line) == Flow::Quit {
                    break;
                }
            }
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(error) => return Err(bridge_error!("terminal error: {error}")),
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::probe::fake::FakeProbe;
    use crate::target::tests::make_config;
    use t32rcl::Value;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lines_split_like_a_shell_except_eval_and_cmd() {
        assert_eq!(
            split_line("reg HSR '\"A B\".C.CR'").unwrap(),
            strings(&["reg", "HSR", "\"A B\".C.CR"])
        );
        assert_eq!(
            split_line("  eval PER.VALUE(\".HSR\")  ").unwrap(),
            strings(&["eval", "PER.VALUE(\".HSR\")"])
        );
        assert_eq!(
            split_line("cmd PRINT \"a b\" --json").unwrap(),
            strings(&["cmd", "PRINT \"a b\"", "--json"])
        );
        assert_eq!(split_line("cmd").unwrap(), strings(&["cmd"]));
        assert!(split_line("reg 'open").is_err());
        assert!(split_line("   ").unwrap().is_empty());
    }

    #[test]
    fn completes_command_names() {
        let names = command_names();
        assert!(names.contains(&"status".to_string()));
        assert!(names.contains(&"exit".to_string()));
        assert_eq!(complete(&names, "st", 2), (0, strings(&["status"])));
        assert_eq!(
            complete(&names, "help re", 7),
            (5, strings(&["reconnect", "reg"]))
        );
        assert_eq!(complete(&names, "reg HS", 6), (6, Vec::new()));
    }

    fn probe(mode: i128, running: Option<bool>) -> FakeProbe {
        let mut probe = FakeProbe::with(&[("SYStem.Mode()", Value::Int(mode))]);
        if let Some(running) = running {
            probe.set("STATE.RUN()", Value::Bool(running));
        }
        probe
    }

    #[test]
    fn prompt_shows_the_debugger_state() {
        let dir = tempfile::tempdir().unwrap();
        let config = make_config(dir.path());
        let connect = || -> DResult<FakeProbe> { Ok(FakeProbe::default()) };
        let mut session = Session::new(
            &config,
            dir.path(),
            false,
            Box::new(connect),
            probe(11, Some(false)),
        );
        assert_eq!(session.prompt(), "t32 [up, halted]> ");
        session.probe = Some(probe(11, Some(true)));
        assert_eq!(session.prompt(), "t32 [up, running]> ");
        session.probe = Some(probe(0, None));
        assert_eq!(session.prompt(), "t32 [down]> ");
        session.probe.as_mut().unwrap().disconnected = true;
        assert_eq!(session.prompt(), "t32 [disconnected]> ");
        assert!(!session.connected());
    }

    #[test]
    fn dispatches_commands_and_survives_errors() {
        let dir = tempfile::tempdir().unwrap();
        let config = make_config(dir.path());
        let mut reconnects = 0;
        let connect = || -> DResult<FakeProbe> {
            reconnects += 1;
            Ok(probe(11, Some(false)))
        };
        let mut session = Session::new(
            &config,
            dir.path(),
            false,
            Box::new(connect),
            probe(11, Some(true)),
        );

        assert_eq!(session.handle_line("break"), Flow::Continue);
        assert_eq!(session.probe.as_ref().unwrap().commands(), ["Break"]);
        // Unknown commands, usage errors and failing commands keep the session.
        assert_eq!(session.handle_line("reset"), Flow::Continue);
        assert_eq!(session.handle_line("mem"), Flow::Continue);
        assert_eq!(session.handle_line("eval NOPE()"), Flow::Continue);
        assert!(session.connected());
        assert_eq!(session.probe.as_ref().unwrap().commands(), ["Break"]);
        assert_eq!(session.handle_line("cmd PRINT \"x\""), Flow::Continue);
        assert_eq!(
            session.probe.as_ref().unwrap().commands(),
            ["Break", "PRINT \"x\""]
        );

        // A lost connection is reported; commands wait for 'reconnect'.
        session.probe.as_mut().unwrap().disconnected = true;
        assert_eq!(session.handle_line("go"), Flow::Continue);
        assert!(!session.connected());
        assert_eq!(session.handle_line("status"), Flow::Continue);
        assert_eq!(session.handle_line("reconnect"), Flow::Continue);
        assert!(session.connected());
        assert_eq!(session.handle_line("help reg"), Flow::Continue);
        assert_eq!(session.handle_line("quit"), Flow::Quit);
        assert_eq!(session.handle_line("exit"), Flow::Quit);
        drop(session);
        assert_eq!(reconnects, 1);
    }
}
