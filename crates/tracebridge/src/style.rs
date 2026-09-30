//! Colours for the human output.
//!
//! A colour marks what a piece of text is (a label, an address, a value, a
//! symbol), so the same kind of thing looks the same in every command. Text
//! is padded before it is painted: the escape sequences have no width, and
//! the columns must line up with and without them.
//!
//! Colours are on when the stream is a terminal. `NO_COLOR` (any non-empty
//! value) turns them off; `CLICOLOR_FORCE` (not empty, not `0`) turns them on
//! for a pipe such as `| less -R`. JSON output is never coloured.
//!
//! Two things stay as they are: the `[tracebridge]` prefix of `ui::info` is
//! always cyan (as in the Python tool), and the log of the `adapter` proxy is
//! always plain, because the IDEs match its lines.

use std::ffi::OsString;
use std::fmt;
use std::io::IsTerminal;

const BOLD: &str = "1";
const DIM: &str = "2";
const RED: &str = "1;31";
const GREEN: &str = "32";
const YELLOW: &str = "33";
const BLUE: &str = "34";
const MAGENTA: &str = "35";
const CYAN: &str = "36";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    enabled: bool,
}

impl Style {
    #[cfg(test)]
    pub const PLAIN: Style = Style { enabled: false };
    #[cfg(test)]
    pub const COLOR: Style = Style { enabled: true };

    fn detect(is_terminal: bool, env: impl Fn(&str) -> Option<OsString>) -> Style {
        let set = |name: &str| env(name).filter(|value| !value.is_empty());
        let enabled = if set("NO_COLOR").is_some() {
            false
        } else if set("CLICOLOR_FORCE").is_some_and(|value| value != "0") {
            true
        } else {
            is_terminal && env("TERM").is_none_or(|term| term != "dumb")
        };
        Style { enabled }
    }

    /// For what a command prints.
    pub fn stdout() -> Style {
        Style::detect(std::io::stdout().is_terminal(), |name| {
            std::env::var_os(name)
        })
    }

    /// For errors and warnings.
    pub fn stderr() -> Style {
        Style::detect(std::io::stderr().is_terminal(), |name| {
            std::env::var_os(name)
        })
    }

    pub fn enabled(self) -> bool {
        self.enabled
    }

    fn paint(self, code: &str, text: impl fmt::Display) -> String {
        let text = text.to_string();
        if !self.enabled || text.is_empty() {
            return text;
        }
        format!("\x1b[{code}m{text}\x1b[0m")
    }

    /// Row labels and register names.
    pub fn label(self, text: impl fmt::Display) -> String {
        self.paint(CYAN, text)
    }

    /// Target addresses (`C15:0x4001`, `AD:0x20000000`).
    pub fn address(self, text: impl fmt::Display) -> String {
        self.paint(BLUE, text)
    }

    /// What was read from the target.
    pub fn value(self, text: impl fmt::Display) -> String {
        self.paint(BOLD, text)
    }

    /// Symbol names.
    pub fn symbol(self, text: impl fmt::Display) -> String {
        self.paint(YELLOW, text)
    }

    /// The meaning of a value (a BITFLD choice).
    pub fn text(self, text: impl fmt::Display) -> String {
        self.paint(GREEN, text)
    }

    /// Notes and explanations.
    pub fn dim(self, text: impl fmt::Display) -> String {
        self.paint(DIM, text)
    }

    pub fn good(self, text: impl fmt::Display) -> String {
        self.paint(GREEN, text)
    }

    pub fn bad(self, text: impl fmt::Display) -> String {
        self.paint(RED, text)
    }

    pub fn warn(self, text: impl fmt::Display) -> String {
        self.paint(YELLOW, text)
    }

    /// A debugger state (`up, running`, `up, halted`, `down`, `disconnected`).
    pub fn state(self, label: &str) -> String {
        let code = if label.ends_with("running") {
            GREEN
        } else if label.ends_with("halted") {
            YELLOW
        } else if label == "disconnected" || label == "?" {
            RED
        } else if label.starts_with("up") {
            GREEN
        } else {
            MAGENTA
        };
        self.paint(code, label)
    }

    /// The session prompt `t32 [<state>]> ` with the state in its colour.
    pub fn prompt(self, prompt: &str) -> String {
        let parts = prompt
            .split_once(" [")
            .and_then(|(name, rest)| Some((name, rest.split_once(']')?)));
        match parts {
            Some((name, (state, tail))) => {
                format!("{} [{}]{tail}", self.value(name), self.state(state))
            }
            None => prompt.to_string(),
        }
    }
}

/// `tracebridge: <message>` on stderr.
pub fn error(message: impl fmt::Display) {
    eprintln!("{} {message}", Style::stderr().bad("tracebridge:"));
}

/// `tracebridge: warning: <message>` on stderr.
pub fn warning(message: impl fmt::Display) {
    eprintln!(
        "{} {message}",
        Style::stderr().warn("tracebridge: warning:")
    );
}

/// `tracebridge: <message>` on stderr, for what is done without being asked.
pub fn notice(message: impl fmt::Display) {
    eprintln!("{} {message}", Style::stderr().warn("tracebridge:"));
}

/// Text without its SGR sequences.
#[cfg(test)]
pub fn strip(text: &str) -> String {
    let mut plain = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("\x1b[") {
        plain.push_str(&rest[..start]);
        let end = rest[start..].find('m').expect("unterminated SGR sequence");
        rest = &rest[start + end + 1..];
    }
    plain.push_str(rest);
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(is_terminal: bool, vars: &[(&str, &str)]) -> bool {
        Style::detect(is_terminal, |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        })
        .enabled()
    }

    #[test]
    fn colours_follow_the_terminal_and_the_environment() {
        assert!(detect(true, &[]));
        assert!(!detect(false, &[]));
        assert!(detect(true, &[("TERM", "xterm-256color")]));
        assert!(!detect(true, &[("TERM", "dumb")]));
        assert!(!detect(true, &[("NO_COLOR", "1")]));
        // An empty value counts as not set.
        assert!(detect(true, &[("NO_COLOR", "")]));
        assert!(detect(false, &[("CLICOLOR_FORCE", "1")]));
        assert!(!detect(false, &[("CLICOLOR_FORCE", "0")]));
        assert!(!detect(false, &[("CLICOLOR_FORCE", "")]));
        assert!(detect(true, &[("CLICOLOR_FORCE", "1"), ("TERM", "dumb")]));
        assert!(!detect(true, &[("CLICOLOR_FORCE", "1"), ("NO_COLOR", "1")]));
    }

    #[test]
    fn plain_style_changes_nothing() {
        assert_eq!(Style::PLAIN.label("HSR"), "HSR");
        assert_eq!(Style::PLAIN.state("up, halted"), "up, halted");
        assert_eq!(
            Style::PLAIN.prompt("t32 [up, halted]> "),
            "t32 [up, halted]> "
        );
    }

    #[test]
    fn paints_and_resets() {
        assert_eq!(Style::COLOR.label("HSR"), "\x1b[36mHSR\x1b[0m");
        assert_eq!(Style::COLOR.value(7), "\x1b[1m7\x1b[0m");
        // Nothing to paint: no stray escape sequences.
        assert_eq!(Style::COLOR.dim(""), "");
        assert_eq!(strip(&Style::COLOR.bad("FAIL")), "FAIL");
    }

    #[test]
    fn states_have_their_own_colours() {
        let style = Style::COLOR;
        assert_eq!(style.state("up, running"), "\x1b[32mup, running\x1b[0m");
        assert_eq!(style.state("up, halted"), "\x1b[33mup, halted\x1b[0m");
        assert_eq!(style.state("up"), "\x1b[32mup\x1b[0m");
        assert_eq!(style.state("down"), "\x1b[35mdown\x1b[0m");
        assert_eq!(style.state("disconnected"), "\x1b[1;31mdisconnected\x1b[0m");
    }

    #[test]
    fn prompt_keeps_its_text() {
        let style = Style::COLOR;
        assert_eq!(
            style.prompt("t32 [up, halted]> "),
            "\x1b[1mt32\x1b[0m [\x1b[33mup, halted\x1b[0m]> "
        );
        for prompt in ["t32 [down]> ", "t32 [disconnected]> ", "t32 [?]> ", "> "] {
            assert_eq!(strip(&style.prompt(prompt)), prompt);
        }
    }
}
