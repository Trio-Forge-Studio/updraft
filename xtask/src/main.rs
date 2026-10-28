mod build;
mod diff;
mod naming;
mod overlay;
#[cfg(test)]
mod tests;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use updraft_registry::{Registry, SpecSource};

const SPEC_REPOSITORY: &str = "Roblox/creator-docs";
const SPEC_PATH: &str = "content/en-us/reference/cloud/openapi.json";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().position(|arg| arg == name).and_then(|at| args.get(at + 1)).cloned();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let spec_path = flag("--spec").map(PathBuf::from).unwrap_or_else(|| root.join("spec/openapi.json"));
    let registry_path = flag("--out").map(PathBuf::from).unwrap_or_else(|| root.join("spec/registry.json"));
    let overlay_path = root.join("spec/overlay.toml");
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["spec", "build", ..] => rebuild(&spec_path, &registry_path, &overlay_path, flag("--commit"), Mode::Write),
        ["spec", "check", ..] => rebuild(&spec_path, &registry_path, &overlay_path, flag("--commit"), Mode::Check),
        ["spec", "update", ..] => update(&spec_path, &registry_path, &overlay_path),
        _ => Err("usage: cargo xtask spec <update|build|check> [--spec path] [--out path] [--commit sha]".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

#[derive(PartialEq)]
enum Mode {
    Write,
    Check,
}

fn rebuild(
    spec_path: &Path,
    registry_path: &Path,
    overlay_path: &Path,
    commit: Option<String>,
    mode: Mode,
) -> Result<(), String> {
    let bytes = fs::read(spec_path).map_err(|e| format!("{}: {e}", spec_path.display()))?;
    let spec: Value = serde_json::from_slice(&bytes).map_err(|e| format!("spec is not JSON: {e}"))?;
    let previous: Option<Registry> = fs::read_to_string(registry_path)
        .ok()
        .map(|text| serde_json::from_str(&text).map_err(|e| format!("{}: {e}", registry_path.display())))
        .transpose()?;
    let overlay: overlay::Overlay = match fs::read_to_string(overlay_path) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", overlay_path.display()))?,
        Err(_) => overlay::Overlay::default(),
    };
    let commit =
        commit.or_else(|| previous.as_ref().map(|registry| registry.source.commit.clone())).unwrap_or_default();
    let source = SpecSource { repository: SPEC_REPOSITORY.to_owned(), commit, sha256: spec_digest(&bytes) };
    let (registry, report) =
        build::build(build::Input { spec: &spec, source, previous: previous.as_ref(), overlay: &overlay })
            .map_err(|errors| errors.join("\n"))?;
    let text = serde_json::to_string_pretty(&registry).map_err(|e| e.to_string())? + "\n";
    let compact = serde_json::to_string(&registry).map_err(|e| e.to_string())?;
    let changes = previous.as_ref().map(|old| diff::diff(old, &registry));

    if mode == Mode::Check {
        let current = fs::read_to_string(registry_path).unwrap_or_default();
        if current != text {
            return Err(format!(
                "spec/registry.json is stale. Run `cargo xtask spec build`.\n{}",
                serde_json::to_string_pretty(&changes).unwrap_or_default()
            ));
        }
        return Ok(());
    }
    fs::write(registry_path, &text).map_err(|e| format!("{}: {e}", registry_path.display()))?;
    let summary = json!({
        "report": report,
        "bytes": { "spec": bytes.len(), "registry_pretty": text.len(), "registry_compact": compact.len() },
        "diff": changes,
    });
    println!("{}", serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())?);
    Ok(())
}

// A Windows checkout can hold CRLF bytes, so hash the text the repository stores.
fn spec_digest(bytes: &[u8]) -> String {
    let mut normalized = Vec::with_capacity(bytes.len());
    for (at, byte) in bytes.iter().enumerate() {
        if *byte != b'\r' || bytes.get(at + 1) != Some(&b'\n') {
            normalized.push(*byte);
        }
    }
    format!("{:x}", Sha256::digest(&normalized))
}

fn update(spec_path: &Path, registry_path: &Path, overlay_path: &Path) -> Result<(), String> {
    let url = format!("https://api.github.com/repos/{SPEC_REPOSITORY}/commits?path={SPEC_PATH}&per_page=1");
    let commit = latest_commit(&url, std::env::var("GITHUB_TOKEN").ok().as_deref())?;
    let bytes = ureq::get(&format!("https://raw.githubusercontent.com/{SPEC_REPOSITORY}/{commit}/{SPEC_PATH}"))
        .call()
        .map_err(|e| format!("fetch spec: {e}"))?
        .body_mut()
        .with_config()
        .limit(32 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("read spec: {e}"))?;
    let _: Value = serde_json::from_slice(&bytes).map_err(|e| format!("downloaded spec is not JSON: {e}"))?;
    let temporary = spec_path.with_extension("json.tmp");
    fs::write(&temporary, &bytes).map_err(|e| format!("write spec: {e}"))?;
    fs::rename(&temporary, spec_path).map_err(|e| format!("replace spec: {e}"))?;
    rebuild(spec_path, registry_path, overlay_path, Some(commit.to_owned()), Mode::Write)
}

fn latest_commit(url: &str, token: Option<&str>) -> Result<String, String> {
    let mut request = ureq::get(url).header("User-Agent", "updraft-spec");
    if let Some(token) = token.filter(|token| !token.is_empty()) {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let commits: Value = request
        .call()
        .map_err(|e| format!("fetch commits: {e}"))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("read commits: {e}"))?;
    commits[0]["sha"]
        .as_str()
        .filter(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| "GitHub returned no commit SHA".into())
        .map(str::to_owned)
}
