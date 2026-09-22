//! Python standard-library behaviour the configuration depends on:
//! `os.path.expanduser`, `os.path.expandvars`, `Path.resolve()` (non-strict),
//! `int(text)` / `int(text, 0)` and `shlex.split`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// A snapshot of the environment (`os.environ`), injectable for tests.
pub type Env = BTreeMap<String, String>;

pub fn process_env() -> Env {
    std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

/// `os.path.expanduser`: `~` and `~/...` use `$HOME`, `~user` the password
/// database; anything else is returned unchanged.
pub fn expanduser(value: &str, env: &Env) -> String {
    if !value.starts_with('~') {
        return value.to_string();
    }
    let end = value.find('/').unwrap_or(value.len());
    let (user, rest) = value.split_at(end);
    let home = if user == "~" {
        env.get("HOME").cloned().or_else(|| home_of(None))
    } else {
        home_of(Some(&user[1..]))
    };
    match home {
        Some(home) => {
            let home = if home == "/" {
                String::new()
            } else {
                home.trim_end_matches('/').to_string()
            };
            let joined = format!("{home}{rest}");
            if joined.is_empty() {
                "/".to_string()
            } else {
                joined
            }
        }
        None => value.to_string(),
    }
}

#[cfg(unix)]
fn home_of(user: Option<&str>) -> Option<String> {
    use nix::unistd::{Uid, User};
    let user = match user {
        Some(name) => User::from_name(name).ok().flatten(),
        None => User::from_uid(Uid::current()).ok().flatten(),
    }?;
    Some(user.dir.to_string_lossy().into_owned())
}

#[cfg(not(unix))]
fn home_of(_user: Option<&str>) -> Option<String> {
    None
}

/// `os.path.expandvars` (POSIX): `$name` and `${name}` are replaced when the
/// variable exists; everything else is left alone.
pub fn expandvars(value: &str, env: &Env) -> String {
    if !value.contains('$') {
        return value.to_string();
    }
    let mut result = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('$') {
        result.push_str(&rest[..index]);
        let after = &rest[index + 1..];
        let (name, consumed) = if let Some(braced) = after.strip_prefix('{') {
            match braced.find('}') {
                Some(end) => (&braced[..end], end + 2),
                None => ("", 0),
            }
        } else {
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            (&after[..end], end)
        };
        match env.get(name) {
            Some(replacement) if consumed > 0 => {
                result.push_str(replacement);
                rest = &after[consumed..];
            }
            _ => {
                result.push('$');
                rest = after;
            }
        }
    }
    result.push_str(rest);
    result
}

/// `_expand_path` in config.py.
pub fn expand_path(value: &str, env: &Env) -> String {
    expandvars(&expanduser(value, env), env)
}

/// `Path(value).resolve()`: make absolute against `base`, resolve symbolic
/// links of the existing part, and normalise the rest lexically, without
/// requiring the path to exist (`os.path.realpath(strict=False)`).
pub fn resolve(path: &Path, base: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    resolve_absolute(&absolute, 0)
}

fn resolve_absolute(path: &Path, depth: usize) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => result.push(prefix.as_os_str()),
            Component::RootDir => result.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            Component::Normal(name) => {
                result.push(name);
                if depth < 40 {
                    if let Ok(target) = std::fs::read_link(&result) {
                        let parent = result.parent().map(Path::to_path_buf).unwrap_or_default();
                        let joined = if target.is_absolute() {
                            target
                        } else {
                            parent.join(target)
                        };
                        result = resolve_absolute(&joined, depth + 1);
                    }
                }
            }
        }
    }
    result
}

/// `int(text)`: decimal with optional sign, surrounding whitespace and single
/// underscores between digits.
pub fn parse_decimal(text: &str) -> Option<i64> {
    t32rcl::parse_int(text, 10).and_then(|value| i64::try_from(value).ok())
}

/// `int(text, 0)`: `0x`, `0o`, `0b` prefixes or a decimal without leading zeros.
pub fn parse_int_auto(text: &str) -> Option<i128> {
    let trimmed = text.trim();
    let (sign, body) = match trimmed.as_bytes().first()? {
        b'-' => ("-", &trimmed[1..]),
        b'+' => ("", &trimmed[1..]),
        _ => ("", trimmed),
    };
    let lower = body.get(..2).map(str::to_ascii_lowercase);
    let (radix, digits) = match lower.as_deref() {
        Some("0x") => (16, &body[2..]),
        Some("0o") => (8, &body[2..]),
        Some("0b") => (2, &body[2..]),
        _ => (10, body),
    };
    let digits = if radix != 10 {
        digits.strip_prefix('_').unwrap_or(digits)
    } else {
        digits
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || digits.starts_with(['+', '-'])
    {
        return None;
    }
    let cleaned: String = digits.chars().filter(|&c| c != '_').collect();
    if radix == 10
        && cleaned.len() > 1
        && cleaned.starts_with('0')
        && cleaned.bytes().any(|b| b != b'0')
    {
        return None;
    }
    let value = i128::from_str_radix(&cleaned, radix).ok()?;
    Some(if sign == "-" { -value } else { value })
}

/// `shlex.split(text)` (POSIX mode, no comments). The error texts are those of
/// Python's `ValueError`.
pub fn shlex_split(text: &str) -> Result<Vec<String>, &'static str> {
    #[derive(PartialEq)]
    enum State {
        Normal,
        Single,
        Double,
    }
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut state = State::Normal;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match state {
            State::Normal => match c {
                ' ' | '\t' | '\r' | '\n' => {
                    if in_token {
                        tokens.push(std::mem::take(&mut current));
                        in_token = false;
                    }
                }
                '\\' => {
                    let escaped = chars.next().ok_or("No escaped character")?;
                    current.push(escaped);
                    in_token = true;
                }
                '\'' => {
                    state = State::Single;
                    in_token = true;
                }
                '"' => {
                    state = State::Double;
                    in_token = true;
                }
                _ => {
                    current.push(c);
                    in_token = true;
                }
            },
            State::Single => match c {
                '\'' => state = State::Normal,
                _ => current.push(c),
            },
            State::Double => match c {
                '"' => state = State::Normal,
                '\\' => {
                    let escaped = chars.next().ok_or("No escaped character")?;
                    if escaped != '"' && escaped != '\\' {
                        current.push('\\');
                    }
                    current.push(escaped);
                }
                _ => current.push(c),
            },
        }
    }
    if state != State::Normal {
        return Err("No closing quotation");
    }
    if in_token {
        tokens.push(current);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn expanduser_uses_home() {
        let env = env(&[("HOME", "/home/me")]);
        assert_eq!(expanduser("~", &env), "/home/me");
        assert_eq!(expanduser("~/t32", &env), "/home/me/t32");
        assert_eq!(expanduser("a/~", &env), "a/~");
        assert_eq!(expanduser("~nosuchuser_tb/x", &env), "~nosuchuser_tb/x");
    }

    #[test]
    fn expandvars_matches_posixpath() {
        let env = env(&[("A", "1"), ("B_2", "two")]);
        assert_eq!(expandvars("$A/${B_2}/$C/${C}", &env), "1/two/$C/${C}");
        assert_eq!(expandvars("$A$A", &env), "11");
        assert_eq!(expandvars("x$", &env), "x$");
        assert_eq!(expandvars("${A", &env), "${A");
        assert_eq!(expandvars("$-A", &env), "$-A");
    }

    #[test]
    fn resolve_is_lexical_for_missing_paths() {
        let base = Path::new("/nonexistent-tb/base");
        assert_eq!(
            resolve(Path::new("../x/./y"), base),
            PathBuf::from("/nonexistent-tb/x/y")
        );
        assert_eq!(resolve(Path::new("/a/b/.."), base), PathBuf::from("/a"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_follows_symbolic_links() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
        let resolved = resolve(Path::new("link/missing"), dir.path());
        let expected = std::fs::canonicalize(&real).unwrap().join("missing");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn python_int_forms() {
        assert_eq!(parse_decimal(" 21000 "), Some(21000));
        assert_eq!(parse_decimal("2_0"), Some(20));
        assert_eq!(parse_decimal("abc"), None);
        assert_eq!(parse_int_auto("0x20001000"), Some(0x2000_1000));
        assert_eq!(parse_int_auto("0X10"), Some(16));
        assert_eq!(parse_int_auto("0b101"), Some(5));
        assert_eq!(parse_int_auto("0o17"), Some(15));
        assert_eq!(parse_int_auto("4096"), Some(4096));
        assert_eq!(parse_int_auto("0"), Some(0));
        assert_eq!(parse_int_auto("010"), None);
        assert_eq!(parse_int_auto("0x"), None);
        assert_eq!(parse_int_auto("zz"), None);
    }

    #[test]
    fn shlex_matches_python() {
        assert_eq!(
            shlex_split(r#"DUALPORT=1 "A B" 'c d' e\ f"#).unwrap(),
            ["DUALPORT=1", "A B", "c d", "e f"]
        );
        assert_eq!(shlex_split(r#"a"b c"d"#).unwrap(), ["ab cd"]);
        assert_eq!(shlex_split(r#""x\"y\\z\n""#).unwrap(), [r#"x"y\z\n"#]);
        assert_eq!(shlex_split("''").unwrap(), [""]);
        assert_eq!(shlex_split("  ").unwrap(), Vec::<String>::new());
        assert_eq!(shlex_split("a #b").unwrap(), ["a", "#b"]);
        assert_eq!(shlex_split("'open").unwrap_err(), "No closing quotation");
        assert_eq!(shlex_split("end\\").unwrap_err(), "No escaped character");
    }
}
