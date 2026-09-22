//! Console output shared by the commands.

use std::path::{Path, PathBuf};

/// `info` in cli.py: a cyan `[tracebridge]` prefix on stdout.
pub fn info(message: &str) {
    println!("\x1b[1;36m[tracebridge]\x1b[0m {message}");
}

/// The absolute path of this executable, used wherever tracebridge calls itself
/// (PowerView toolbar, VS Code task, RustRover run configuration).
///
/// A Homebrew install runs from `<prefix>/Cellar/tracebridge/<version>/bin`,
/// which `brew upgrade` deletes; the stable `<prefix>/opt/tracebridge/bin`
/// link is used instead so generated files keep working after upgrades.
pub fn current_exe() -> crate::errors::Result<PathBuf> {
    let exe = std::env::current_exe().map_err(|error| {
        crate::bridge_error!("cannot determine the tracebridge executable: {error}")
    })?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Ok(homebrew_stable_path(&exe)
        .filter(|path| path.is_file())
        .unwrap_or(exe))
}

/// `<prefix>/Cellar/<name>/<version>/bin/<exe>` → `<prefix>/opt/<name>/bin/<exe>`.
pub fn homebrew_stable_path(exe: &Path) -> Option<PathBuf> {
    let file = exe.file_name()?;
    let bin = exe.parent()?;
    let version = bin.parent()?;
    let name = version.parent()?;
    let cellar = name.parent()?;
    if bin.file_name()? != "bin" || cellar.file_name()? != "Cellar" {
        return None;
    }
    Some(
        cellar
            .parent()?
            .join("opt")
            .join(name.file_name()?)
            .join("bin")
            .join(file),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn homebrew_cellar_path_maps_to_opt() {
        assert_eq!(
            homebrew_stable_path(Path::new(
                "/opt/homebrew/Cellar/tracebridge/0.1.1/bin/tracebridge"
            )),
            Some(PathBuf::from(
                "/opt/homebrew/opt/tracebridge/bin/tracebridge"
            ))
        );
        assert_eq!(
            homebrew_stable_path(Path::new(
                "/home/linuxbrew/.linuxbrew/Cellar/tracebridge/0.2.0/bin/tracebridge"
            )),
            Some(PathBuf::from(
                "/home/linuxbrew/.linuxbrew/opt/tracebridge/bin/tracebridge"
            ))
        );
        assert_eq!(
            homebrew_stable_path(Path::new("/Users/me/.local/bin/tracebridge")),
            None
        );
        assert_eq!(
            homebrew_stable_path(Path::new("/x/target/release/tracebridge")),
            None
        );
    }
}
