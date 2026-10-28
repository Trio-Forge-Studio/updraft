use crate::call::{BodyInput, Call, Source};
use crate::exec::{Executor, decode_captures};
use crate::outcome::Failure;
use serde_json::{Value, json};
use std::path::PathBuf;
#[derive(clap::Args)]
pub struct Args {
    pub script: String,
    #[arg(long,value_parser=super::positive_id)]
    pub universe: String,
    #[arg(long,value_parser=super::positive_id)]
    pub place: String,
    #[arg(long)]
    pub version: Option<u64>,
    #[arg(long)]
    pub timeout: Option<u64>,
}
pub fn primary<'r>(r: &'r updraft_registry::Registry, args: &Args) -> &'r updraft_registry::Op {
    super::op(r, if args.version.is_some() { super::LUAU_VERSION } else { super::LUAU })
}
pub fn run(ex: &Executor<'_>, args: Args) -> Result<Value, Failure> {
    let op = primary(ex.registry, &args);
    let source = if args.script == "-" { Source::Stdin } else { Source::File(PathBuf::from(&args.script)) };
    let bytes = source.read()?;
    if bytes.len() > 4_000_000 {
        return Err(Failure::usage("Luau script exceeds 4 MB"));
    }
    let script = String::from_utf8(bytes).map_err(|_| Failure::usage("Luau script is not UTF-8"))?;
    let mut body = json!({"script":script});
    if let Some(timeout) = args.timeout {
        body["timeout"] = json!(format!("{timeout}s"));
    }
    let mut pairs = vec![("universe_id".into(), args.universe), ("place_id".into(), args.place)];
    if let Some(version) = args.version {
        pairs.push(("version_id".into(), version.to_string()));
    }
    let input = BodyInput::Json {
        content_type: "application/json".into(),
        bytes: serde_json::to_vec(&body).expect("a JSON value serializes"),
    };
    let first = ex.send(&Call::new(op, vec![], pairs, Some(input))?)?.json()?;
    if !first["path"].as_str().is_some_and(|path| !path.is_empty()) {
        return Err(Failure::unknown("Luau create returned no task path"));
    }
    let (task, failed) = match ex.finish(op, first) {
        Ok(task) => (task, false),
        Err(f) if f.code == 6 => (f.error["detail"].clone(), true),
        Err(f) => return Err(f),
    };
    let path = task["path"].as_str().ok_or_else(|| Failure::unavailable("task has no path"))?;
    let log_result = logs(ex, op, path);
    let mut detail = json!({"task":path,"state":task["state"],"results":task["output"]["results"].as_array().cloned().unwrap_or_default(),"logs":[],"logsComplete":true});
    match log_result {
        Ok(logs) => detail["logs"] = json!(logs),
        Err(error) => {
            detail["logsComplete"] = json!(false);
            detail["logError"] = error.error;
            if !failed {
                return Err(Failure::unavailable("task completed but logs could not be read").with("detail", detail));
            }
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
        .ok_or_else(|| Failure::unavailable(format!("no reader matches task path {path}")).with("path", json!(path)))?;
    let result = ex.send_all(Call::new(op, decode_captures(captures.0), vec![], None)?, None)?;
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
