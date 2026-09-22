//! `tracebridge init`: write a commented trace32.toml into the current directory.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::bail;
use crate::config::CONFIG_FILE_NAME;
use crate::errors::Result;

const TEMPLATE: &str = include_str!("../assets/trace32.toml");

/// The template with the program name derived from the directory name.
pub fn render(directory: &Path) -> String {
    let name: String = directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let name = if name.is_empty() {
        "app".to_string()
    } else {
        name
    };
    TEMPLATE.replace("__PROGRAM__", &name)
}

/// Create trace32.toml in `directory`; never overwrite an existing file.
pub fn init(directory: &Path) -> Result<PathBuf> {
    let path = directory.join(CONFIG_FILE_NAME);
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!(
                "{} already exists; refusing to overwrite it",
                path.display()
            )
        }
        Err(error) => bail!("cannot create {}: {error}", path.display()),
    };
    file.write_all(render(directory).as_bytes())
        .map_err(|error| crate::bridge_error!("cannot write {}: {error}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load_config;
    use crate::pycompat::Env;

    #[test]
    fn template_loads_and_uses_directory_name() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("my app");
        std::fs::create_dir(&project).unwrap();
        let path = init(&project).unwrap();
        let config = load_config(&path, &Env::new()).unwrap();
        assert_eq!(config.program, "my_app");
        assert!(config.elf.ends_with("build/my_app.elf"));
        assert_eq!(config.flash_args, ["DUALPORT=1", "JTAG_CLOCK=10MHz"]);
        assert!(config.rtos_show_tasks);
    }

    #[test]
    fn existing_file_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE_NAME), "keep").unwrap();
        let error = init(dir.path()).unwrap_err();
        assert!(error.0.contains("refusing to overwrite"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join(CONFIG_FILE_NAME)).unwrap(),
            "keep"
        );
    }
}
