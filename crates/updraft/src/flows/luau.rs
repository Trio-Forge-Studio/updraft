use crate::call::{BodyInput, Call, Source};
use crate::exec::{Executor, decode_captures};
use crate::outcome::Failure;
use serde_json::{Value, json};
use std::path::PathBuf;
#[derive(clap::Args)]
pub struct Args {
    /// A UTF-8 Luau file, or - to read stdin.
    #[arg(required_unless_present = "resume", conflicts_with = "resume")]
    pub script: Option<String>,
    /// Resume an accepted task resource path using GET only.
    #[arg(long, value_parser = task_path, conflicts_with_all = ["script", "universe", "place", "version", "timeout"])]
    pub resume: Option<String>,
    /// The universe ID where the script runs.
    #[arg(long, required_unless_present = "resume", value_parser = super::positive_id)]
    pub universe: Option<String>,
    /// The place ID where the script runs.
    #[arg(long, required_unless_present = "resume", value_parser = super::positive_id)]
    pub place: Option<String>,
    /// Run against this place version instead of the latest.
    #[arg(long)]
    pub version: Option<u64>,
    /// Set the Roblox execution timeout in seconds.
    #[arg(long)]
    pub timeout: Option<u64>,
}
fn task_path(path: &str) -> Result<String, String> {
    if path.starts_with("universes/")
        && path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_'))
        && path.split('/').all(|part| !part.is_empty())
    {
        Ok(path.into())
    } else {
        Err("--resume requires a task resource path".into())
    }
}
const READER: &str = "GET /cloud/v2/universes/{universe_id}/places/{place_id}/versions/{version_id}/luau-execution-sessions/{luau_execution_session_id}/tasks/{task_id}";
pub fn primary<'r>(r: &'r updraft_registry::Registry, args: &Args) -> &'r updraft_registry::Op {
    super::op(
        r,
        if args.resume.is_some() {
            READER
        } else if args.version.is_some() {
            super::LUAU_VERSION
        } else {
            super::LUAU
        },
    )
}
fn first(ex: &Executor<'_>, args: Args) -> Result<(Value, &'static updraft_registry::Op), Failure> {
    let op = primary(ex.registry, &args);
    if let Some(path) = args.resume {
        return Ok((json!({"path":path}), op));
    }
    let script = args.script.expect("clap requires a script for creation");
    let source = if script == "-" { Source::Stdin } else { Source::File(PathBuf::from(script)) };
    let bytes = source.read()?;
    if bytes.len() > 4_000_000 {
        return Err(Failure::usage("Luau script exceeds 4 MB"));
    }
    let script = String::from_utf8(bytes).map_err(|_| Failure::usage("Luau script is not UTF-8"))?;
    let mut body = json!({"script":script});
    if let Some(timeout) = args.timeout {
        body["timeout"] = json!(format!("{timeout}s"));
    }
    let mut pairs = vec![
        ("universe_id".into(), args.universe.expect("clap requires universe")),
        ("place_id".into(), args.place.expect("clap requires place")),
    ];
    if let Some(version) = args.version {
        pairs.push(("version_id".into(), version.to_string()));
    }
    let input = BodyInput::Json {
        content_type: "application/json".into(),
        bytes: serde_json::to_vec(&body).expect("a JSON value serializes"),
    };
    let first = ex.send(&Call::new(op, vec![], pairs, Some(input))?)?.json()?;
    if let Some(path) = first["path"].as_str() {
        task_path(path).map_err(|_| Failure::unknown("Luau create returned an invalid task path"))?;
    }
    Ok((first, op))
}
pub fn run(ex: &Executor<'_>, args: Args) -> Result<Value, Failure> {
    let (first, op) = first(ex, args)?;
    let resume = first["path"]
        .as_str()
        .filter(|path| ex.registry.reader_of(op, path).is_ok())
        .map(|path| format!("updraft luau run --resume {path}"));
    if let Some(resume) = resume {
        ex.resume(resume);
    }
    let (task, failed) = match ex.finish(op, first) {
        Ok(task) => (task, false),
        Err(f) if f.code == 6 => (f.error["detail"].clone(), true),
        Err(f) => return Err(f),
    };
    let path = task["path"].as_str();
    let log_result = path
        .ok_or_else(|| Failure::unavailable("task has no path for reading logs"))
        .and_then(|path| logs(ex, op, path));
    let mut detail = json!({"task":path,"state":task["state"],"results":task["output"]["results"].as_array().cloned().unwrap_or_default(),"logs":[],"logsComplete":true});
    match log_result {
        Ok(logs) => detail["logs"] = json!(logs),
        Err(error) => {
            detail["logsComplete"] = json!(false);
            detail["logError"] = error.error;
            ex.note("Warning: task logs could not be read; logsComplete is false.");
        }
    }
    if failed {
        Err(Failure::remote(task["error"]["message"].as_str().unwrap_or("Luau task failed or was cancelled"), detail))
    } else {
        Ok(detail)
    }
}
fn logs(ex: &Executor<'_>, from: &updraft_registry::Op, path: &str) -> Result<Vec<String>, Failure> {
    let log_path = format!("{path}/logs");
    let (op, captures) = ex
        .registry
        .reader_of(from, &log_path)
        .map_err(|_| Failure::unavailable(format!("no reader matches task path {path}")).with("path", json!(path)))?;
    let result = ex.send_all(Call::new(op, decode_captures(captures.0), vec![], None)?, None, 100)?;
    if result["nextPageToken"].as_str().is_some_and(|token| !token.is_empty()) {
        return Err(Failure::unavailable("task logs exceeded 100 pages"));
    }
    let entries = result["luauExecutionSessionTaskLogs"]
        .as_array()
        .ok_or_else(|| Failure::unavailable("logs response has no log array"))?;
    Ok(entries
        .iter()
        .flat_map(|entry| entry["messages"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect())
}
