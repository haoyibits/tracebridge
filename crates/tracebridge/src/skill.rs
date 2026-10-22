//! `tracebridge skill`: the agent skill that tells an AI agent which work goes
//! to tracebridge and which to Lauterbach's t32mcp.
//!
//! t32mcp and the `lauterbach-*` skills cover generic debugging; their
//! instructions also tell the agent not to drive the debugger by other means.
//! This skill states the user's split: flashing, project setup and the custom
//! analyses run through the tracebridge CLI. It is written to
//! `<skills>/tracebridge/SKILL.md`, by default Claude Code's user skills
//! directory, and carries the tracebridge version so `config` can tell when it
//! is outdated.

use std::fs;
use std::path::{Path, PathBuf};

use crate::bridge_error;
use crate::errors::Result;
use crate::pycompat::{Env, expanduser};
use crate::style::Style;

const TEMPLATE: &str = include_str!("../assets/skill.md");

/// The skill's directory name, equal to `name` in its frontmatter.
const NAME: &str = "tracebridge";

pub fn render() -> String {
    TEMPLATE.replace("{version}", env!("CARGO_PKG_VERSION"))
}

/// Claude Code's user skills: `$CLAUDE_CONFIG_DIR/skills`, else `~/.claude/skills`.
pub fn default_dir(env: &Env) -> PathBuf {
    env.get("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(expanduser("~/.claude", env)))
        .join("skills")
}

pub fn skill_file(skills_dir: &Path) -> PathBuf {
    skills_dir.join(NAME).join("SKILL.md")
}

/// `metadata.version` of an installed skill file.
pub fn installed_version(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let frontmatter = text.strip_prefix("---\n")?.split("\n---").next()?;
    frontmatter.lines().find_map(|line| {
        let value = line.trim().strip_prefix("version:")?;
        Some(value.trim().trim_matches('"').to_string())
    })
}

/// Write `<skills_dir>/tracebridge/SKILL.md`, backing up a different existing file.
pub fn install(skills_dir: &Path) -> Result<PathBuf> {
    let path = skill_file(skills_dir);
    let directory = path.parent().expect("the skill file has a directory");
    fs::create_dir_all(directory)
        .map_err(|error| bridge_error!("cannot create {}: {error}", directory.display()))?;
    let contents = render();
    let style = Style::stdout();
    if path.exists() {
        if fs::read_to_string(&path).is_ok_and(|current| current == contents) {
            println!("{} {}", style.dim("unchanged"), path.display());
            return Ok(path);
        }
        let backup = crate::vscode::installer::backup(&path)?;
        println!(
            "{} {} -> {}",
            style.dim("backed up"),
            path.display(),
            backup.display()
        );
    }
    crate::vscode::installer::atomic_write(&path, &contents)?;
    println!("{} {}", style.good("installed"), path.display());
    println!(
        "\nNew agent sessions load it. Generic debugging goes through Lauterbach's t32mcp, \
         which must use the same Remote API port as trace32.toml (trace32.rcl_port)."
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_skill_names_itself_and_carries_the_version() {
        let text = render();
        assert!(text.starts_with("---\nname: tracebridge\ndescription: "));
        assert!(text.contains(&format!("version: \"{}\"", env!("CARGO_PKG_VERSION"))));
        assert!(!text.contains("{version}"));
        let description = text
            .lines()
            .find_map(|line| line.strip_prefix("description: "))
            .unwrap();
        // The Agent Skills limit for a description.
        assert!(description.len() <= 1024, "{}", description.len());
    }

    #[test]
    fn install_is_idempotent_and_backs_up_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = install(dir.path()).unwrap();
        assert_eq!(path, dir.path().join("tracebridge/SKILL.md"));
        assert_eq!(
            installed_version(&path).as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        install(dir.path()).unwrap();
        let count = || {
            fs::read_dir(dir.path().join("tracebridge"))
                .unwrap()
                .count()
        };
        assert_eq!(count(), 1);
        fs::write(
            &path,
            "---\nname: tracebridge\nmetadata:\n  version: \"0.0.1\"\n---\n",
        )
        .unwrap();
        assert_eq!(installed_version(&path).as_deref(), Some("0.0.1"));
        install(dir.path()).unwrap();
        assert_eq!(count(), 2);
        assert_eq!(fs::read_to_string(&path).unwrap(), render());
    }

    #[test]
    fn version_only_from_the_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("SKILL.md");
        fs::write(&path, "no frontmatter\nversion: 1\n").unwrap();
        assert_eq!(installed_version(&path), None);
        fs::write(&path, "---\nname: x\n---\nversion: 1\n").unwrap();
        assert_eq!(installed_version(&path), None);
        assert_eq!(installed_version(&dir.path().join("missing")), None);
    }

    #[test]
    fn default_dir_follows_claude_config_dir() {
        let home: Env = [("HOME".to_string(), "/home/me".to_string())].into();
        assert_eq!(default_dir(&home), PathBuf::from("/home/me/.claude/skills"));
        let mut custom = home.clone();
        custom.insert("CLAUDE_CONFIG_DIR".into(), "/cfg/claude".into());
        assert_eq!(default_dir(&custom), PathBuf::from("/cfg/claude/skills"));
        custom.insert("CLAUDE_CONFIG_DIR".into(), String::new());
        assert_eq!(
            default_dir(&custom),
            PathBuf::from("/home/me/.claude/skills")
        );
    }
}
