// SPDX-License-Identifier: GPL-3.0-only
/*
 * qtfblight - QTFB to libblight compatibility layer
 * Copyright (C) 2026 Noa Himesaka
 */

//! Convert an AppLoad `external.manifest.json` into an Oxide application
//! registration. QTFB applications are hosted through qtfblight; other
//! external applications use Oxide's standard runner.

use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

const DEFAULT_QTFBLIGHT: &str = "/home/root/.vellum/bin/qtfblight";
const DEFAULT_OXIDE_RUNNER: &str = "/home/root/.vellum/share/oxide/libexec/runner";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExternalManifest {
    name: String,
    application: String,
    #[serde(default)]
    working_directory: Option<String>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    qtfb: bool,
    #[serde(default)]
    aspect_ratio: Option<String>,
    #[serde(default)]
    disables_windowed_mode: Option<bool>,
}

struct Options {
    input: PathBuf,
    output: Option<PathBuf>,
    qtfblight: PathBuf,
    oxide_runner: PathBuf,
}

fn usage() -> &'static str {
    "Usage: appload-to-oxide [OPTIONS] <APP_DIRECTORY_OR_MANIFEST>\n\
\n\
Convert an AppLoad external manifest to an Oxide .oxide registration.\n\
\n\
Options:\n\
  -o, --output <PATH>       Write JSON to PATH instead of stdout\n\
      --qtfblight <PATH>    qtfblight binary (default: /home/root/qtfblight)\n\
      --oxide-runner <PATH> Oxide runner for non-QTFB apps\n\
                             (default: /home/root/.vellum/share/oxide/libexec/runner)\n\
  -h, --help                Show this help"
}

fn parse_args() -> Result<Options, String> {
    let mut args = env::args_os().skip(1);
    let mut input = None;
    let mut output = None;
    let mut qtfblight = PathBuf::from(DEFAULT_QTFBLIGHT);
    let mut oxide_runner = PathBuf::from(DEFAULT_OXIDE_RUNNER);

    while let Some(argument) = args.next() {
        match argument.to_string_lossy().as_ref() {
            "-h" | "--help" => {
                println!("{}", usage());
                process::exit(0);
            }
            "-o" | "--output" => {
                output = Some(PathBuf::from(args.next().ok_or_else(|| {
                    format!("{} requires a path", argument.to_string_lossy())
                })?));
            }
            "--qtfblight" => {
                qtfblight = PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--qtfblight requires a path".to_string())?,
                );
            }
            "--oxide-runner" => {
                oxide_runner = PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--oxide-runner requires a path".to_string())?,
                );
            }
            value if value.starts_with('-') => return Err(format!("unknown option: {value}")),
            _ if input.is_some() => {
                return Err("only one app directory or manifest may be provided".to_string());
            }
            _ => input = Some(PathBuf::from(argument)),
        }
    }

    Ok(Options {
        input: input.ok_or_else(|| {
            "missing AppLoad app directory or external.manifest.json path".to_string()
        })?,
        output,
        qtfblight,
        oxide_runner,
    })
}

fn resolve_path(app_dir: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        app_dir.join(path)
    }
}

fn shell_quote(argument: &str) -> String {
    format!("'{}'", argument.replace('\'', "'\"'\"'"))
}

fn shell_command(application: &Path, args: &[String]) -> String {
    std::iter::once(application.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .map(|argument| shell_quote(&argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_arguments(args: &[String]) -> String {
    args.iter()
        .map(|argument| shell_quote(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_koreader(manifest: &ExternalManifest) -> bool {
    manifest.name.trim().eq_ignore_ascii_case("koreader")
        || Path::new(&manifest.application)
            .file_stem()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("koreader"))
}

fn uses_qtfb_shim(manifest: &ExternalManifest) -> bool {
    manifest
        .environment
        .keys()
        .any(|key| key.starts_with("QTFB_SHIM_"))
        || manifest
            .environment
            .get("LD_PRELOAD")
            .is_some_and(|preload| {
                preload
                    .split(|character| character == ':' || character == ' ')
                    .any(|entry| {
                        matches!(
                            Path::new(entry).file_name().and_then(|name| name.to_str()),
                            Some("qtfb-shim.so" | "qtfb-shim-32bit.so")
                        )
                    })
            })
}

fn skip_reason(manifest: &ExternalManifest) -> Option<&'static str> {
    if is_koreader(manifest) {
        Some("KOReader is supplied with native Blight support; no registration was generated")
    } else if uses_qtfb_shim(manifest) {
        Some("Oxide imports AppLoad QTFB-shim applications itself; no registration was generated")
    } else {
        None
    }
}

fn translate(
    manifest: ExternalManifest,
    app_dir: &Path,
    qtfblight: &Path,
    oxide_runner: &Path,
) -> Value {
    let application = resolve_path(app_dir, &manifest.application);
    let working_directory = manifest
        .working_directory
        .as_deref()
        .map(|directory| resolve_path(app_dir, directory))
        .unwrap_or_else(|| app_dir.to_path_buf());
    let mut environment = manifest.environment;

    let bin = if manifest.qtfb {
        environment.insert(
            "_QTFBLIGHT_COMMAND".to_string(),
            shell_command(&application, &manifest.args),
        );
        qtfblight
    } else {
        environment.insert(
            "EXECUTABLE".to_string(),
            application.to_string_lossy().into_owned(),
        );
        if !manifest.args.is_empty() {
            environment.insert("ARGUMENTS".to_string(), shell_arguments(&manifest.args));
        }
        oxide_runner
    };

    let mut registration = Map::new();
    registration.insert("displayName".to_string(), Value::String(manifest.name));
    registration.insert(
        "bin".to_string(),
        Value::String(bin.to_string_lossy().into_owned()),
    );
    registration.insert("type".to_string(), Value::String("foreground".to_string()));
    registration.insert("flags".to_string(), json!(["nopreload"]));
    registration.insert(
        "workingDirectory".to_string(),
        Value::String(working_directory.to_string_lossy().into_owned()),
    );
    registration.insert("environment".to_string(), json!(environment));

    let icon = app_dir.join("icon.png");
    if icon.is_file() {
        registration.insert(
            "icon".to_string(),
            Value::String(icon.to_string_lossy().into_owned()),
        );
    }

    Value::Object(registration)
}

fn warn_untranslated(manifest: &ExternalManifest) {
    if manifest.aspect_ratio.is_some() {
        eprintln!("warning: AppLoad aspectRatio has no Oxide registration equivalent; omitted");
    }
    if manifest.disables_windowed_mode.is_some() {
        eprintln!(
            "warning: AppLoad disablesWindowedMode has no Oxide registration equivalent; omitted"
        );
    }
}

fn run() -> Result<(), String> {
    let options = parse_args()?;
    let requested_manifest = if options.input.is_dir() {
        options.input.join("external.manifest.json")
    } else {
        options.input
    };
    let manifest_path = requested_manifest.canonicalize().map_err(|error| {
        format!(
            "failed to resolve {}: {error}",
            requested_manifest.display()
        )
    })?;
    let contents = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("failed to read {}: {error}", manifest_path.display()))?;
    let manifest: ExternalManifest = serde_json::from_str(&contents)
        .map_err(|error| format!("invalid AppLoad manifest: {error}"))?;
    if manifest.name.trim().is_empty() || manifest.application.trim().is_empty() {
        return Err("AppLoad manifest requires non-empty name and application fields".to_string());
    }
    if let Some(reason) = skip_reason(&manifest) {
        eprintln!(
            "appload-to-oxide: skipping {}: {reason}",
            manifest_path.display()
        );
        return Ok(());
    }

    let app_dir = manifest_path
        .parent()
        .ok_or_else(|| "manifest has no parent directory".to_string())?;
    warn_untranslated(&manifest);
    let registration = translate(manifest, app_dir, &options.qtfblight, &options.oxide_runner);
    let output = serde_json::to_string_pretty(&registration)
        .map_err(|error| format!("failed to serialize Oxide registration: {error}"))?;

    if let Some(path) = options.output {
        fs::write(&path, format!("{output}\n"))
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    } else {
        println!("{output}");
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("appload-to-oxide: {error}\n\n{}", usage());
        process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(qtfb: bool) -> ExternalManifest {
        ExternalManifest {
            name: "Example app".to_string(),
            application: "bin/example".to_string(),
            working_directory: Some("runtime".to_string()),
            environment: BTreeMap::from([("EXAMPLE".to_string(), "value".to_string())]),
            args: vec!["--message".to_string(), "it's working".to_string()],
            qtfb,
            aspect_ratio: None,
            disables_windowed_mode: None,
        }
    }

    #[test]
    fn qtfb_manifest_uses_qtfblight_wrapper() {
        let registration = translate(
            manifest(true),
            Path::new("/apps/example"),
            Path::new("/opt/qtfblight"),
            Path::new("/opt/runner"),
        );

        assert_eq!(registration["bin"], "/opt/qtfblight");
        assert_eq!(registration["type"], "foreground");
        assert_eq!(registration["flags"], json!(["nopreload"]));
        assert_eq!(registration["workingDirectory"], "/apps/example/runtime");
        assert_eq!(registration["environment"]["EXAMPLE"], "value");
        assert_eq!(
            registration["environment"]["_QTFBLIGHT_COMMAND"],
            "'/apps/example/bin/example' '--message' 'it'\"'\"'s working'"
        );
        assert!(registration.get("icon").is_none());
    }

    #[test]
    fn non_qtfb_manifest_uses_oxide_runner() {
        let registration = translate(
            manifest(false),
            Path::new("/apps/example"),
            Path::new("/opt/qtfblight"),
            Path::new("/opt/runner"),
        );

        assert_eq!(registration["bin"], "/opt/runner");
        assert_eq!(
            registration["environment"]["EXECUTABLE"],
            "/apps/example/bin/example"
        );
        assert_eq!(
            registration["environment"]["ARGUMENTS"],
            "'--message' 'it'\"'\"'s working'"
        );
    }

    #[test]
    fn qtfb_shim_manifest_is_left_for_oxide() {
        let mut shim_manifest = manifest(true);
        shim_manifest.environment.insert(
            "LD_PRELOAD".to_string(),
            "/home/root/shims/qtfb-shim-32bit.so".to_string(),
        );
        assert!(uses_qtfb_shim(&shim_manifest));
        assert_eq!(
            skip_reason(&shim_manifest),
            Some(
                "Oxide imports AppLoad QTFB-shim applications itself; no registration was generated"
            )
        );
    }

    #[test]
    fn koreader_manifest_is_left_for_native_blight_support() {
        let mut koreader_manifest = manifest(true);
        koreader_manifest.name = "KOReader".to_string();
        assert_eq!(
            skip_reason(&koreader_manifest),
            Some("KOReader is supplied with native Blight support; no registration was generated")
        );
    }
}
