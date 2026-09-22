//! `tracebridge rustrover`: a shared run configuration for JetBrains IDEs.
//!
//! RustRover has no built-in DAP client; the LSP4IJ plugin (Red Hat) adds a
//! "Debug Adapter Protocol" run configuration type (`DAPConfiguration`). The
//! file written here is `.run/TRACE32 Attach.run.xml`, which the IDE picks up
//! as a shared run configuration. It runs `tracebridge adapter` in LSP4IJ's
//! launch mode: LSP4IJ starts the command, waits for the proxy's ready line
//! and connects to the address and port in it. Launch mode sends a `launch`
//! request; the proxy turns it into the `attach` t32debugadapter expects.

use std::fs;
use std::path::{Path, PathBuf};

use crate::bridge_error;
use crate::config::Config;
use crate::errors::Result;

pub const RUN_CONFIGURATION: &str = "TRACE32 Attach.run.xml";

/// Files the IDE allows breakpoints in (LSP4IJ "Mappings" tab).
const FILE_PATTERNS: [&str; 8] = ["*.c", "*.h", "*.cpp", "*.hpp", "*.cc", "*.s", "*.S", "*.rs"];

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\n', "&#10;")
}

/// Quote one argument for LSP4IJ's command line (split like a shell command).
fn quote(argument: &str) -> String {
    if !argument.is_empty()
        && argument
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:=@%,".contains(c))
    {
        argument.to_string()
    } else {
        format!(
            "\"{}\"",
            argument.replace('\\', "\\\\").replace('"', "\\\"")
        )
    }
}

pub fn render(config: &Config, exe: &Path) -> String {
    let command = [
        quote(&exe.to_string_lossy()),
        "--config".to_string(),
        quote(&config.config_file.to_string_lossy()),
        "adapter".to_string(),
    ]
    .join(" ");
    let parameters = format!(
        "{{\n  \"type\": \"node\",\n  \"request\": \"attach\",\n  \"trace32Node\": \"localhost\",\n  \"trace32Port\": {}\n}}",
        config.rcl_port
    );
    let option = |name: &str, value: &str| {
        format!(
            "    <option name=\"{name}\" value=\"{}\" />\n",
            xml_escape(value)
        )
    };
    let mut xml = String::from(
        "<component name=\"ProjectRunConfigurationManager\">\n  \
         <configuration default=\"false\" name=\"TRACE32: Attach\" type=\"DAPConfiguration\" \
         factoryName=\"DAPConfiguration\">\n",
    );
    xml.push_str(&option("serverName", "tracebridge"));
    xml.push_str(&option("command", &command));
    xml.push_str(&option(
        "workingDirectory",
        &config.project_dir.to_string_lossy(),
    ));
    xml.push_str(&option("debugMode", "LAUNCH"));
    xml.push_str(&option("debugServerWaitStrategy", "TRACE"));
    xml.push_str(&option(
        "debugServerReadyPattern",
        "[tracebridge] adapter listening on ${address}:${port}",
    ));
    xml.push_str(&option(
        "connectTimeout",
        &(config.dap_backend_timeout * 1000 + 5000).to_string(),
    ));
    xml.push_str(&option("launchConfiguration", &parameters));
    xml.push_str("    <option name=\"serverMappings\">\n      <list>\n        <ServerMappingSettings>\n          <fileType>\n");
    for pattern in FILE_PATTERNS {
        xml.push_str(&format!("            <option value=\"{pattern}\" />\n"));
    }
    xml.push_str(
        "          </fileType>\n        </ServerMappingSettings>\n      </list>\n    </option>\n",
    );
    xml.push_str("    <method v=\"2\" />\n  </configuration>\n</component>\n");
    xml
}

/// Write `.run/TRACE32 Attach.run.xml`, backing up a different existing file.
pub fn install(config: &Config, exe: &Path) -> Result<PathBuf> {
    let directory = config.project_dir.join(".run");
    fs::create_dir_all(&directory)
        .map_err(|error| bridge_error!("cannot create {}: {error}", directory.display()))?;
    let path = directory.join(RUN_CONFIGURATION);
    let contents = render(config, exe);
    if path.exists() {
        if fs::read_to_string(&path).is_ok_and(|current| current == contents) {
            println!("unchanged {}", path.display());
            return Ok(path);
        }
        let backup = crate::vscode::installer::backup(&path)?;
        println!("backed up {} -> {}", path.display(), backup.display());
    }
    crate::vscode::installer::atomic_write(&path, &contents)?;
    println!("installed {}", path.display());
    println!(
        "\nDone. In RustRover: install the \"LSP4IJ\" plugin (Settings > Plugins), then pick \
         'TRACE32: Attach' in the run configurations and press Debug. Run flash, load and rtt \
         from a terminal."
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_configuration_starts_the_adapter_in_launch_mode() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::target::tests::make_config(dir.path());
        let xml = render(&config, Path::new("/Users/me/.local/bin/tracebridge"));
        assert!(xml.contains("type=\"DAPConfiguration\""));
        assert!(xml.contains(&format!(
            "<option name=\"command\" value=\"/Users/me/.local/bin/tracebridge --config {} adapter\" />",
            config.config_file.display()
        )));
        assert!(xml.contains("<option name=\"debugMode\" value=\"LAUNCH\" />"));
        assert!(xml.contains("<option name=\"debugServerWaitStrategy\" value=\"TRACE\" />"));
        assert!(xml.contains("value=\"[tracebridge] adapter listening on ${address}:${port}\""));
        assert!(xml.contains("&quot;request&quot;: &quot;attach&quot;"));
        assert!(xml.contains("&quot;trace32Port&quot;: 20000"));
        assert!(xml.contains("<option value=\"*.c\" />"));
    }

    #[test]
    fn ready_pattern_matches_the_proxy_line_like_lsp4ij() {
        // LSP4IJ: static text must start the remaining input, ${address} is
        // [\w.]+ and ${port} is \d+.
        let line = "[tracebridge] adapter listening on 127.0.0.1:58870 (backend 58871)";
        let rest = line
            .strip_prefix("[tracebridge] adapter listening on ")
            .unwrap();
        let (address, rest) = rest.split_once(':').unwrap();
        assert!(
            address
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        );
        let port: String = rest.chars().take_while(char::is_ascii_digit).collect();
        assert_eq!(port, "58870");
    }

    #[test]
    fn paths_with_spaces_are_quoted_and_escaped() {
        assert_eq!(quote("/a b/tracebridge"), "\"/a b/tracebridge\"");
        assert_eq!(quote("/a/b"), "/a/b");
        assert_eq!(xml_escape("a\"<&>"), "a&quot;&lt;&amp;&gt;");
    }

    #[test]
    fn install_is_idempotent_and_backs_up_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::target::tests::make_config(dir.path());
        let exe = Path::new("/usr/local/bin/tracebridge");
        let path = install(&config, exe).unwrap();
        assert_eq!(path, dir.path().join(".run").join(RUN_CONFIGURATION));
        install(&config, exe).unwrap();
        let count = || fs::read_dir(dir.path().join(".run")).unwrap().count();
        assert_eq!(count(), 1);
        config.rcl_port = 21000;
        install(&config, exe).unwrap();
        assert_eq!(count(), 2);
        assert!(fs::read_to_string(&path).unwrap().contains("21000"));
    }
}
