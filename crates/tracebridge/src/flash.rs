//! Choosing the flash script.
//!
//! `flash.script` names a script explicitly. Otherwise the script is chosen by
//! chip name (`flash.chip`, or `target.cpu` when that is empty) from:
//!
//! 1. the user's library, `~/.config/tracebridge/flash/*.cmm` (or
//!    `$XDG_CONFIG_HOME/tracebridge/flash`), for scripts shared by several
//!    projects, such as a modified vendor script;
//! 2. the TRACE32 installation, `<trace32.sys>/demo/*/flash/*.cmm`.
//!
//! A script is a candidate when its header has an `@Chip:` line (Lauterbach's
//! convention, patterns such as `STM32H7*`) that matches the chip, and it
//! supports `PREPAREONLY`. The library wins over the installation; within one
//! source an exact pattern wins over a wildcard, and a longer wildcard over a
//! shorter one. On a tie the internal-flash script wins over variants for
//! other memories: Lauterbach names those `<family>-<memory>.cmm` (`-spi`,
//! `-qspi`, `-emmc`, `-optionbyte`, ...). A remaining tie is reported instead
//! of guessed.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::errors::Result;
use crate::pycompat::Env;
use crate::{bail, bridge_error};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Trace32,
    Library,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Library => "library",
            Source::Trace32 => "TRACE32",
        }
    }
}

/// A flash script with the chip patterns from its header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub path: PathBuf,
    pub source: Source,
    pub chips: Vec<String>,
    pub prepare_only: bool,
    /// The script takes the derivative as `CPU=<name>` (Lauterbach's family
    /// scripts do; without it they fall back to a default derivative).
    pub accepts_cpu: bool,
}

/// How the flash script was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// `flash.script` (a path or a `~~/` TRACE32 path).
    Explicit(String),
    /// Chosen for `chip` by the pattern `pattern`.
    Chip {
        chip: String,
        pattern: String,
        script: Script,
    },
}

impl Choice {
    /// `CPU=<chip>` to add to the script arguments: the script was chosen by
    /// chip, takes `CPU=`, and `args` do not set it already.
    pub fn cpu_argument(&self, args: &[String]) -> Option<String> {
        match self {
            Choice::Chip { chip, script, .. }
                if script.accepts_cpu
                    && !args
                        .iter()
                        .any(|arg| arg.to_ascii_uppercase().starts_with("CPU=")) =>
            {
                Some(format!("CPU={chip}"))
            }
            _ => None,
        }
    }

    /// The script as passed to `DO`.
    pub fn script(&self) -> String {
        match self {
            Choice::Explicit(script) => script.clone(),
            Choice::Chip { script, .. } => script.path.to_string_lossy().into_owned(),
        }
    }
}

/// `~/.config/tracebridge/flash`, honouring `XDG_CONFIG_HOME`.
pub fn library_dir(env: &Env) -> PathBuf {
    let base = env
        .get("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(crate::pycompat::expanduser("~/.config", env)));
    base.join("tracebridge").join("flash")
}

/// Case-insensitive glob match supporting `*` and `?`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.to_ascii_uppercase().chars().collect();
    let text: Vec<char> = text.to_ascii_uppercase().chars().collect();
    let (mut p, mut t) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(position) = star {
            p = position + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// How specific a matching pattern is: exact names first, then the number of
/// literal characters.
fn specificity(pattern: &str) -> (bool, usize) {
    let literal = pattern.chars().filter(|c| !matches!(c, '*' | '?')).count();
    (literal == pattern.chars().count(), literal)
}

/// Read the `@Chip:` patterns and `PREPAREONLY` support of one script.
pub fn read_script(path: &Path, source: Source) -> Option<Script> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut chips = Vec::new();
    for line in text.lines().take(200) {
        let line = line.trim_start_matches([';', ' ', '\t']);
        if let Some(patterns) = line.strip_prefix("@Chip:") {
            chips.extend(
                patterns
                    .split([' ', '\t', ','])
                    .filter(|pattern| !pattern.is_empty())
                    .map(str::to_string),
            );
        }
    }
    let upper = text.to_ascii_uppercase();
    Some(Script {
        path: path.to_path_buf(),
        source,
        chips,
        prepare_only: upper.contains("PREPAREONLY"),
        accepts_cpu: upper.contains("\"CPU=\""),
    })
}

fn cmm_files(directory: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("cmm"))
        })
        .collect();
    files.sort();
    files
}

/// Every script in the library and in `<sys>/demo/*/flash`.
pub fn catalog(config: &Config, env: &Env) -> Vec<Script> {
    let mut scripts: Vec<Script> = cmm_files(&library_dir(env))
        .iter()
        .filter_map(|path| read_script(path, Source::Library))
        .collect();
    let mut architectures: Vec<PathBuf> = std::fs::read_dir(config.t32_sys.join("demo"))
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path().join("flash")))
        .filter(|path| path.is_dir())
        .collect();
    architectures.sort();
    for directory in architectures {
        scripts.extend(
            cmm_files(&directory)
                .iter()
                .filter_map(|path| read_script(path, Source::Trace32)),
        );
    }
    scripts
}

/// The best pattern of `script` that matches `chip`.
fn best_pattern<'a>(script: &'a Script, chip: &str) -> Option<&'a str> {
    script
        .chips
        .iter()
        .filter(|pattern| glob_match(pattern, chip))
        .max_by_key(|pattern| specificity(pattern))
        .map(String::as_str)
}

/// Pick the script for `chip` from `scripts`.
pub fn choose(chip: &str, scripts: &[Script]) -> std::result::Result<(String, Script), String> {
    type Rank = (Source, (bool, usize), bool);
    let mut candidates: Vec<(Rank, &str, &Script)> = scripts
        .iter()
        .filter(|script| script.prepare_only)
        .filter_map(|script| {
            best_pattern(script, chip).map(|pattern| {
                (
                    (script.source, specificity(pattern), !is_variant(script)),
                    pattern,
                    script,
                )
            })
        })
        .collect();
    if candidates.is_empty() {
        let unusable: Vec<String> = scripts
            .iter()
            .filter(|script| !script.prepare_only && best_pattern(script, chip).is_some())
            .map(|script| script.path.display().to_string())
            .collect();
        let note = if unusable.is_empty() {
            String::new()
        } else {
            format!(
                " ({} match but do not support PREPAREONLY)",
                unusable.join(", ")
            )
        };
        return Err(format!("no flash script found for chip {chip}{note}"));
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    let best = candidates[0].0;
    let top: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.0 == best)
        .collect();
    if top.len() > 1 {
        let list: Vec<String> = top
            .iter()
            .map(|(_, pattern, script)| format!("{} ({pattern})", script.path.display()))
            .collect();
        return Err(format!(
            "chip {chip} matches several flash scripts equally well: {}",
            list.join(", ")
        ));
    }
    Ok((top[0].1.to_string(), top[0].2.clone()))
}

/// `<family>-<memory>.cmm`: a script for external or special memory.
fn is_variant(script: &Script) -> bool {
    script
        .path
        .file_stem()
        .is_some_and(|stem| stem.to_string_lossy().contains('-'))
}

/// The chip used to choose a script: `flash.chip`, else `target.cpu`.
pub fn chip_name(config: &Config) -> Option<&str> {
    [config.flash_chip.as_str(), config.cpu.as_str()]
        .into_iter()
        .find(|name| !name.is_empty())
}

/// Resolve the flash script for `tracebridge flash`.
pub fn resolve(config: &Config, env: &Env) -> Result<Choice> {
    if !config.flash_script.is_empty() {
        return Ok(Choice::Explicit(config.resolved_flash_script()));
    }
    let Some(chip) = chip_name(config) else {
        bail!(
            "no flash script: set flash.chip (or target.cpu) or flash.script in trace32.toml; \
             use load for RAM images"
        );
    };
    let (pattern, script) = choose(chip, &catalog(config, env)).map_err(|error| {
        bridge_error!(
            "{error}; put a script with '; @Chip: {chip}' in its header into {} or set \
             flash.script (see 'tracebridge chips {chip}')",
            library_dir(env).display()
        )
    })?;
    if script.path.to_string_lossy().contains(['"', '\n', '\r']) {
        bail!(
            "flash script path is unsafe for TRACE32 commands: {}",
            script.path.display()
        );
    }
    Ok(Choice::Chip {
        chip: chip.to_string(),
        pattern,
        script,
    })
}

/// Scripts related to `query` for `tracebridge chips`: those whose patterns
/// match it, plus patterns and file names containing it.
pub fn search<'a>(query: &str, scripts: &'a [Script]) -> Vec<&'a Script> {
    let upper = query.to_ascii_uppercase();
    scripts
        .iter()
        .filter(|script| {
            best_pattern(script, query).is_some()
                || script
                    .chips
                    .iter()
                    .any(|pattern| pattern.to_ascii_uppercase().contains(&upper))
                || script.path.file_stem().is_some_and(|stem| {
                    stem.to_string_lossy().to_ascii_uppercase().contains(&upper)
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(name: &str, source: Source, chips: &[&str], prepare_only: bool) -> Script {
        Script {
            path: PathBuf::from(format!("/{}/{name}.cmm", source.name())),
            source,
            chips: chips.iter().map(|c| c.to_string()).collect(),
            prepare_only,
            accepts_cpu: true,
        }
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("SR6P7*", "SR6P7-G7"));
        assert!(glob_match("stm32h7*", "STM32H743ZI"));
        assert!(glob_match("STM32F4?1*", "STM32F401RE"));
        assert!(glob_match("SR6P6", "sr6p6"));
        assert!(!glob_match("SR6P7*", "SR6P6"));
        assert!(!glob_match("SR6P6", "SR6P6X"));
        assert!(glob_match("*", "anything"));
    }

    #[test]
    fn exact_and_longer_patterns_win() {
        let scripts = [
            script("stm32h7", Source::Trace32, &["STM32H7*"], true),
            script("stm32h743", Source::Trace32, &["STM32H743*"], true),
            script("stm32h743xi", Source::Trace32, &["STM32H743XI"], true),
        ];
        assert_eq!(
            choose("STM32H743XI", &scripts).unwrap().1.path,
            scripts[2].path
        );
        assert_eq!(
            choose("STM32H743ZI", &scripts).unwrap().1.path,
            scripts[1].path
        );
        let (pattern, chosen) = choose("STM32H750VB", &scripts).unwrap();
        assert_eq!(
            (pattern.as_str(), &chosen.path),
            ("STM32H7*", &scripts[0].path)
        );
    }

    #[test]
    fn internal_flash_script_wins_over_memory_variants() {
        let scripts = [
            script("stm32f4xx-qspi", Source::Trace32, &["STM32F4*"], true),
            script("stm32f4xx", Source::Trace32, &["STM32F4*"], true),
            script("stm32f4xx-optionbyte", Source::Trace32, &["STM32F4*"], true),
        ];
        assert_eq!(
            choose("STM32F407VG", &scripts).unwrap().1.path,
            scripts[1].path
        );
    }

    #[test]
    fn library_wins_over_trace32() {
        let scripts = [
            script("sr6p6", Source::Trace32, &["SR6P6"], true),
            script("my_sr6", Source::Library, &["SR6*"], true),
        ];
        assert_eq!(choose("SR6P6", &scripts).unwrap().1.source, Source::Library);
    }

    #[test]
    fn ties_and_missing_prepareonly_are_reported() {
        let scripts = [
            script("a", Source::Trace32, &["STM32F4*"], true),
            script("b", Source::Trace32, &["STM32F4*"], true),
        ];
        let error = choose("STM32F407VG", &scripts).unwrap_err();
        assert!(error.contains("several flash scripts"), "{error}");
        let scripts = [script("old", Source::Trace32, &["SR6P6"], false)];
        let error = choose("SR6P6", &scripts).unwrap_err();
        assert!(error.contains("do not support PREPAREONLY"), "{error}");
        assert_eq!(
            choose("XYZ", &[]).unwrap_err(),
            "no flash script found for chip XYZ"
        );
    }

    #[test]
    fn header_patterns_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.cmm");
        std::fs::write(
            &path,
            "; @Title: x\n; @Chip: STM32H7S* STM32H7R*\n;@Chip: STM32H750VB\nENTRY %LINE &p\n\
             &p=STRing.SCAN(\"&p\",\"PREPAREONLY\",0)\n\
             &c=STRing.SCANAndExtract(\"&p\",\"CPU=\",\"\")\n",
        )
        .unwrap();
        let script = read_script(&path, Source::Library).unwrap();
        assert_eq!(script.chips, ["STM32H7S*", "STM32H7R*", "STM32H750VB"]);
        assert!(script.prepare_only);
        assert!(script.accepts_cpu);
    }

    fn config_with(dir: &Path) -> (Config, Env) {
        let mut config = crate::target::tests::make_config(dir);
        config.flash_script = String::new();
        config.t32_sys = dir.join("t32");
        let flash = dir.join("t32/demo/arm/flash");
        std::fs::create_dir_all(&flash).unwrap();
        std::fs::write(
            flash.join("sr6p7g7.cmm"),
            "; @Chip: SR6P7*\n; DO sr6p7g7 [PREPAREONLY]\n",
        )
        .unwrap();
        let env: Env = [(
            "XDG_CONFIG_HOME".to_string(),
            dir.join("config").display().to_string(),
        )]
        .into();
        (config, env)
    }

    #[test]
    fn resolution_order() {
        let dir = tempfile::tempdir().unwrap();
        let (mut config, env) = config_with(dir.path());

        // target.cpu is used when flash.chip is empty.
        config.cpu = "SR6P7-G7".into();
        let choice = resolve(&config, &env).unwrap();
        assert_eq!(
            choice.script(),
            dir.path()
                .join("t32/demo/arm/flash/sr6p7g7.cmm")
                .to_string_lossy()
        );

        // flash.chip wins over target.cpu; the library over TRACE32.
        config.flash_chip = "SR6P6".into();
        let error = resolve(&config, &env).unwrap_err();
        assert!(
            error
                .0
                .starts_with("no flash script found for chip SR6P6; put a script"),
            "{error}"
        );
        let library = library_dir(&env);
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(library.join("sr6p6.cmm"), "; @Chip: SR6P6\n; PREPAREONLY\n").unwrap();
        match resolve(&config, &env).unwrap() {
            Choice::Chip {
                pattern, script, ..
            } => {
                assert_eq!(pattern, "SR6P6");
                assert_eq!(script.source, Source::Library);
            }
            other => panic!("{other:?}"),
        }

        // flash.script wins over everything.
        config.flash_script = "~~/demo/arm/flash/other.cmm".into();
        assert_eq!(
            resolve(&config, &env).unwrap(),
            Choice::Explicit("~~/demo/arm/flash/other.cmm".into())
        );

        // Nothing to go on.
        config.flash_script.clear();
        config.flash_chip.clear();
        config.cpu.clear();
        assert!(
            resolve(&config, &env)
                .unwrap_err()
                .0
                .starts_with("no flash script:")
        );
    }

    #[test]
    fn cpu_argument_is_added_only_when_useful() {
        let chosen = |accepts_cpu| Choice::Chip {
            chip: "STM32F407VG".into(),
            pattern: "STM32F4*".into(),
            script: Script {
                accepts_cpu,
                ..script("stm32f4xx", Source::Trace32, &["STM32F4*"], true)
            },
        };
        assert_eq!(
            chosen(true).cpu_argument(&["DUALPORT=1".into()]),
            Some("CPU=STM32F407VG".into())
        );
        assert_eq!(chosen(true).cpu_argument(&["cpu=STM32F405RG".into()]), None);
        assert_eq!(chosen(false).cpu_argument(&[]), None);
        assert_eq!(Choice::Explicit("x.cmm".into()).cpu_argument(&[]), None);
    }

    #[test]
    fn search_finds_related_scripts() {
        let scripts = [
            script("stm32f4", Source::Trace32, &["STM32F4*"], true),
            script("sr6p7g7", Source::Trace32, &["SR6P7*"], true),
        ];
        assert_eq!(search("STM32F407", &scripts).len(), 1);
        assert_eq!(search("sr6", &scripts).len(), 1);
        assert_eq!(search("nrf", &scripts).len(), 0);
    }
}
