//! `tracebridge vscode` (vscode/installer.py): merge the debug configuration
//! into .vscode/launch.json and .vscode/tasks.json.
//!
//! Compared with the Python tool only the debugging pieces remain: the
//! "TRACE32: Attach" launch configuration and the hidden task that starts the
//! adapter. Flash, load and RTT are CLI commands, so the old visible tasks
//! are removed when the files are merged.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::jsonc;
use crate::config::Config;
use crate::errors::Result;
use crate::{bail, bridge_error};

const TASKS_TEMPLATE: &str = include_str!("../../assets/tasks.json");
const LAUNCH_TEMPLATE: &str = include_str!("../../assets/launch.json");

/// Tasks written by the Python tool; they are dropped when merging.
pub const LEGACY_TASK_LABELS: [&str; 6] = [
    "T32: Flash",
    "T32: Load ELF",
    "T32: RTT Viewer",
    "T32: Start Debug Adapter",
    "T32: Flash + Debug",
    "T32: Load + Debug",
];
/// `LEGACY_LAUNCH_NAMES`.
pub const LEGACY_LAUNCH_NAMES: [&str; 2] = ["1. Flash + Debug", "2. Load + Debug"];

/// `replace_tokens`: a string that is exactly a token becomes the token's
/// value (so ports stay numbers); tokens inside longer strings are replaced
/// by their text.
pub fn replace_tokens(value: &Value, replacements: &[(&str, Value)]) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| replace_tokens(item, replacements))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), replace_tokens(item, replacements)))
                .collect(),
        ),
        Value::String(text) => {
            if let Some((_, replacement)) = replacements.iter().find(|(token, _)| token == text) {
                return replacement.clone();
            }
            let mut result = text.clone();
            for (token, replacement) in replacements {
                let text = match replacement {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                result = result.replace(token, &text);
            }
            Value::String(result)
        }
        other => other.clone(),
    }
}

/// `merge_named_items`: template items replace existing items with the same
/// name (keeping extra keys of the existing item), other existing items are
/// kept in place, `ignored` names are dropped, new template items are appended.
pub fn merge_named_items(
    existing: &[Value],
    template: &[Value],
    key: &str,
    ignored: &[&str],
) -> Vec<Value> {
    let name_of = |item: &Value| item.get(key).and_then(Value::as_str).map(str::to_string);
    let mut installed: Vec<String> = Vec::new();
    let mut merged = Vec::new();
    for item in existing {
        let name = name_of(item);
        if name.as_deref().is_some_and(|name| ignored.contains(&name)) {
            continue;
        }
        let replacement = name.as_deref().and_then(|name| {
            template
                .iter()
                .find(|t| name_of(t).as_deref() == Some(name))
        });
        match (replacement, name) {
            (None, _) | (_, None) => merged.push(item.clone()),
            (Some(replacement), Some(name)) => {
                if !installed.contains(&name) {
                    let mut combined = item.as_object().cloned().unwrap_or_default();
                    if let Some(fields) = replacement.as_object() {
                        for (field, value) in fields {
                            combined.insert(field.clone(), value.clone());
                        }
                    }
                    merged.push(Value::Object(combined));
                    installed.push(name);
                }
            }
        }
    }
    for item in template {
        if !name_of(item).is_some_and(|name| installed.contains(&name)) {
            merged.push(item.clone());
        }
    }
    merged
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Tasks,
    Launch,
}

/// `merge_document`.
pub fn merge_document(kind: Kind, existing: &Value, template: &Value) -> Result<Value> {
    let name = match kind {
        Kind::Tasks => "tasks",
        Kind::Launch => "launch",
    };
    let (Some(existing), Some(template)) = (existing.as_object(), template.as_object()) else {
        bail!("{name}.json must contain a JSON object");
    };
    let (list, key, ignored, default_version): (&str, &str, &[&str], &str) = match kind {
        Kind::Tasks => ("tasks", "label", &LEGACY_TASK_LABELS, "2.0.0"),
        Kind::Launch => ("configurations", "name", &LEGACY_LAUNCH_NAMES, "0.2.0"),
    };
    let (Some(existing_items), Some(template_items)) = (
        existing.get(list).and_then(Value::as_array),
        template.get(list).and_then(Value::as_array),
    ) else {
        bail!("{name}.json must contain a {list} array");
    };
    let merged_items = merge_named_items(existing_items, template_items, key, ignored);
    let mut merged: Map<String, Value> = existing.clone();
    merged.insert(
        "version".into(),
        template
            .get("version")
            .cloned()
            .unwrap_or_else(|| Value::String(default_version.into())),
    );
    merged.insert(list.into(), Value::Array(merged_items));
    Ok(Value::Object(merged))
}

/// `backup`: copy to `<name>.bak.<local timestamp>[.<n>]`, keeping permissions
/// and the modification time.
pub fn backup(path: &Path) -> Result<PathBuf> {
    let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let mut candidate = path.with_file_name(format!("{name}.bak.{timestamp}"));
    let mut suffix = 0;
    while candidate.exists() {
        suffix += 1;
        candidate = path.with_file_name(format!("{name}.bak.{timestamp}.{suffix}"));
    }
    let failed =
        |error: std::io::Error| bridge_error!("cannot back up {}: {error}", path.display());
    fs::copy(path, &candidate).map_err(failed)?;
    let modified = fs::metadata(path).and_then(|metadata| metadata.modified());
    if let Ok(modified) = modified {
        let _ = fs::File::options()
            .write(true)
            .open(&candidate)
            .and_then(|file| file.set_modified(modified));
    }
    Ok(candidate)
}

/// `json.dumps(document, indent=4, ensure_ascii=False) + "\n"`.
pub fn to_pretty_json(document: &Value) -> String {
    let mut output = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut output, formatter);
    serde::Serialize::serialize(document, &mut serializer).expect("JSON values always serialize");
    let mut text = String::from_utf8(output).expect("serde_json writes UTF-8");
    text.push('\n');
    text
}

/// `atomic_write_json`: write a temporary file next to `path`, then rename.
pub fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = path.with_file_name(format!("{name}.tmp.{}", std::process::id()));
    let result = fs::File::create(&temporary)
        .and_then(|mut file| file.write_all(contents.as_bytes()))
        .and_then(|()| fs::rename(&temporary, path));
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        bail!("cannot update {}: {error}", path.display());
    }
    Ok(())
}

/// The template placeholders and their values.
pub fn replacements(config: &Config, exe: &Path) -> Vec<(&'static str, Value)> {
    vec![
        (
            "__TRACEBRIDGE_EXE__",
            Value::String(exe.to_string_lossy().into_owned()),
        ),
        (
            "__TRACEBRIDGE_CONFIG__",
            Value::String(config.config_file.to_string_lossy().into_owned()),
        ),
        ("__T32_DAP_PORT__", Value::from(config.dap_port)),
        ("__T32_RCL_PORT__", Value::from(config.rcl_port)),
    ]
}

/// Merge one file; returns the backup path when an existing file was replaced.
fn install_file(
    kind: Kind,
    target: &Path,
    template: &str,
    replacements: &[(&str, Value)],
) -> Result<Option<PathBuf>> {
    let empty = match kind {
        Kind::Tasks => serde_json::json!({"version": "2.0.0", "tasks": []}),
        Kind::Launch => serde_json::json!({"version": "0.2.0", "configurations": []}),
    };
    let existing = if target.exists() {
        jsonc::load(target)?
    } else {
        empty
    };
    let template = replace_tokens(&jsonc::loads(template, "template")?, replacements);
    let merged = merge_document(kind, &existing, &template)?;
    let backup_path = if target.exists() {
        let backup_path = backup(target)?;
        println!(
            "backed up {} -> {}",
            target.display(),
            backup_path.display()
        );
        Some(backup_path)
    } else {
        None
    };
    atomic_write(target, &to_pretty_json(&merged))?;
    println!("installed/merged {}", target.display());
    Ok(backup_path)
}

/// `install`.
pub fn install(config: &Config, exe: &Path) -> Result<()> {
    let directory = config.project_dir.join(".vscode");
    fs::create_dir_all(&directory)
        .map_err(|error| bridge_error!("cannot create {}: {error}", directory.display()))?;
    let replacements = replacements(config, exe);
    install_file(
        Kind::Tasks,
        &directory.join("tasks.json"),
        TASKS_TEMPLATE,
        &replacements,
    )?;
    install_file(
        Kind::Launch,
        &directory.join("launch.json"),
        LAUNCH_TEMPLATE,
        &replacements,
    )?;
    println!(
        "\nDone. In VS Code open Run and Debug and start 'TRACE32: Attach' (F5); it starts \
         the adapter through the hidden 'tracebridge: adapter' task. Run flash, load and rtt \
         from a terminal."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // test_vscode_installer.py: test_merge_tasks_preserves_unrelated_and_updates_trace32
    #[test]
    fn merge_tasks_preserves_unrelated_and_updates_tracebridge() {
        let existing = json!({"version": "2.0.0", "tasks": [
            {"label": "build", "command": "make"},
            {"label": "T32: Flash + Debug", "command": "old"},
            {"label": "tracebridge: adapter", "command": "old", "extra": 1},
            {"label": "T32: Start Debug Adapter", "command": "python3"}
        ]});
        let template = json!({"version": "2.0.0", "tasks": [
            {"label": "tracebridge: adapter", "command": "/bin/tracebridge"}
        ]});
        let merged = merge_document(Kind::Tasks, &existing, &template).unwrap();
        assert_eq!(
            merged,
            json!({"version": "2.0.0", "tasks": [
                {"label": "build", "command": "make"},
                {"label": "tracebridge: adapter", "command": "/bin/tracebridge", "extra": 1}
            ]})
        );
    }

    #[test]
    fn merge_launch_drops_legacy_and_appends_new() {
        let existing = json!({"configurations": [
            {"name": "1. Flash + Debug"},
            {"name": "Native", "type": "cppdbg"},
            {"name": "TRACE32: Attach", "preLaunchTask": "T32: Start Debug Adapter", "cmm": "x.cmm"}
        ], "compounds": []});
        let template = json!({"version": "0.2.0", "configurations": [
            {"name": "TRACE32: Attach", "preLaunchTask": "tracebridge: adapter"}
        ]});
        let merged = merge_document(Kind::Launch, &existing, &template).unwrap();
        assert_eq!(
            merged,
            json!({"configurations": [
                {"name": "Native", "type": "cppdbg"},
                {"name": "TRACE32: Attach", "preLaunchTask": "tracebridge: adapter", "cmm": "x.cmm"}
            ], "compounds": [], "version": "0.2.0"})
        );
    }

    #[test]
    fn duplicate_existing_entries_are_merged_once() {
        let existing = [json!({"label": "a", "x": 1}), json!({"label": "a", "x": 2})];
        let template = [json!({"label": "a", "y": 3})];
        assert_eq!(
            merge_named_items(&existing, &template, "label", &[]),
            [json!({"label": "a", "x": 1, "y": 3})]
        );
    }

    #[test]
    fn malformed_documents_are_rejected() {
        let template = json!({"tasks": []});
        assert_eq!(
            merge_document(Kind::Tasks, &json!([]), &template)
                .unwrap_err()
                .0,
            "tasks.json must contain a JSON object"
        );
        assert_eq!(
            merge_document(Kind::Tasks, &json!({"tasks": {}}), &template)
                .unwrap_err()
                .0,
            "tasks.json must contain a tasks array"
        );
        assert_eq!(
            merge_document(Kind::Launch, &json!({}), &json!({"configurations": []}))
                .unwrap_err()
                .0,
            "launch.json must contain a configurations array"
        );
    }

    // test_vscode_installer.py: test_replace_tokens_preserves_number_type
    #[test]
    fn replace_tokens_preserves_number_type() {
        let result = replace_tokens(
            &json!({"debugServer": "__PORT__", "text": "port=__PORT__", "list": ["__PORT__"]}),
            &[("__PORT__", json!(58870))],
        );
        assert_eq!(
            result,
            json!({"debugServer": 58870, "text": "port=58870", "list": [58870]})
        );
    }

    #[test]
    fn templates_resolve_to_the_expected_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::target::tests::make_config(dir.path());
        let replacements = replacements(&config, Path::new("/usr/local/bin/tracebridge"));
        let tasks = replace_tokens(&jsonc::loads(TASKS_TEMPLATE, "t").unwrap(), &replacements);
        let task = &tasks["tasks"][0];
        assert_eq!(task["label"], "tracebridge: adapter");
        assert_eq!(task["command"], "/usr/local/bin/tracebridge");
        assert_eq!(
            task["args"],
            json!(["--config", config.config_file.to_string_lossy(), "adapter"])
        );
        let background = &task["problemMatcher"][0]["background"];
        assert_eq!(
            background["endsPattern"],
            "^\\[tracebridge\\] adapter listening on 127\\.0\\.0\\.1:58870.*$"
        );
        let launch = replace_tokens(&jsonc::loads(LAUNCH_TEMPLATE, "l").unwrap(), &replacements);
        let attach = &launch["configurations"][0];
        assert_eq!(attach["debugServer"], 58870);
        assert_eq!(attach["trace32Port"], 20000);
        assert_eq!(attach["preLaunchTask"], task["label"]);
    }

    #[test]
    fn ready_line_matches_the_task_patterns() {
        // The adapter prints this line (dap::proxy) and the info line (main.rs).
        let ready = "[tracebridge] adapter listening on 127.0.0.1:58870 (backend 58871)";
        let ends = "^\\[tracebridge\\] adapter listening on 127\\.0\\.0\\.1:";
        assert!(ends.starts_with('^'));
        assert!(ready.starts_with(&ends[1..].replace('\\', "")));
        assert!(TASKS_TEMPLATE.contains("starting debug adapter proxy"));
    }

    #[test]
    fn pretty_json_matches_python_indentation() {
        let text = to_pretty_json(&json!({"a": [1, {"b": "ü"}], "e": [], "o": {}}));
        assert_eq!(
            text,
            "{\n    \"a\": [\n        1,\n        {\n            \"b\": \"ü\"\n        }\n    ],\n    \"e\": [],\n    \"o\": {}\n}\n"
        );
    }

    // test_vscode_installer.py: test_install_creates_backup_and_merges_templates
    #[test]
    fn install_creates_backup_and_merges_templates() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::target::tests::make_config(dir.path());
        let vscode = dir.path().join(".vscode");
        fs::create_dir_all(&vscode).unwrap();
        let tasks = vscode.join("tasks.json");
        fs::write(
            &tasks,
            "{\n  // mine\n  \"version\": \"2.0.0\",\n  \"tasks\": [{\"label\": \"build\"},\n  {\"label\": \"T32: Flash\"},],\n}\n",
        )
        .unwrap();
        install(&config, Path::new("/opt/bin/tracebridge")).unwrap();

        let installed = jsonc::load(&tasks).unwrap();
        let labels: Vec<&str> = installed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| task["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["build", "tracebridge: adapter"]);
        assert_eq!(installed["tasks"][1]["command"], "/opt/bin/tracebridge");
        let backups: Vec<_> = fs::read_dir(&vscode)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("tasks.json.bak.")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert!(
            fs::read_to_string(backups[0].path())
                .unwrap()
                .contains("// mine")
        );
        let launch = jsonc::load(&vscode.join("launch.json")).unwrap();
        assert_eq!(launch["configurations"][0]["name"], "TRACE32: Attach");
        // No temporary files are left behind.
        assert!(!fs::read_dir(&vscode).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.")
        }));

        // Running again is stable and creates another backup.
        install(&config, Path::new("/opt/bin/tracebridge")).unwrap();
        assert_eq!(jsonc::load(&tasks).unwrap(), installed);
    }

    #[test]
    fn backup_names_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        fs::write(&path, "{}").unwrap();
        let first = backup(&path).unwrap();
        let second = backup(&path).unwrap();
        assert_ne!(first, second);
        assert!(
            second.to_string_lossy().ends_with(".1") || first.file_name() != second.file_name()
        );
    }
}
