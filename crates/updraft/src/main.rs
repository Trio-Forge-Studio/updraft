mod auth;
mod call;
mod cli;
mod discover;
mod exec;
mod flows;
mod outcome;
mod output;

use crate::call::{BodyInput, Call, Source};
use crate::cli::{AssetCommand, AuthCommand, Cli, Command, LuauCommand, PlaceCommand};
use crate::exec::{Executor, Timing};
use crate::outcome::Failure;
use crate::output::Output;
use clap::Parser;
use serde_json::{Value, json};
use std::process::ExitCode;
use std::sync::LazyLock;
use std::time::Duration;
use updraft_registry::{Auth, Method, OpRef, Registry, Resolved};

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../spec/registry.json")).expect("embedded registry is valid")
});
fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let output = Output::new(false);
            if !error.use_stderr() {
                return if output.text(&error.to_string()).is_ok() { ExitCode::SUCCESS } else { ExitCode::from(7) };
            }
            let failure = Failure::usage(error.kind().to_string());
            let _ = output.document(&failure.document(), true);
            return ExitCode::from(2);
        }
    };
    let mut output = Output::new(cli.verbose);
    let (code, document) = match run(cli, &mut output) {
        Ok(v) => (0, v),
        Err(f) => (f.code, f.document()),
    };
    if output.document(&document, code != 0).is_err() {
        return ExitCode::from(7);
    }
    ExitCode::from(code)
}
fn run(cli: Cli, output: &mut Output) -> Result<Value, Failure> {
    let registry = &*REGISTRY;
    match &cli.command {
        Command::Find { query, limit } => return Ok(json!(registry.find(&query.join(" "), *limit))),
        Command::Describe { operation } => return Ok(discover::describe(registry, op_ref(registry, operation)?.0.op)),
        _ => {}
    }
    let prepared = if let Command::Call(args) = &cli.command {
        let (resolved, used) = op_ref(registry, &args.words)?;
        let parts = call::parse_pairs(&args.parts)?.into_iter().map(|(k, v)| (k, Source::parse(&v))).collect();
        let body = call::read_body(resolved.op, args.body.as_deref().map(Source::parse), parts)?;
        if args.all && resolved.op.pagination.is_none() {
            return Err(Failure::usage("operation does not declare pagination"));
        }
        Some(Call::new(resolved.op, resolved.preset, call::parse_pairs(&args.words[used..])?, body)?)
    } else {
        None
    };
    let config = auth::load_config()?;
    if matches!(cli.command, Command::Auth(AuthCommand::Profiles)) {
        return Ok(json!(config.profiles.iter().map(|(name,p)|json!({"name":name,"keyEnv":p.key_env,"keyFile":p.key_file,"group":p.group,"user":p.user,"present":auth::load(&config,&auth::KeySource::Profile(name.clone())).is_ok()})).collect::<Vec<_>>()));
    }
    let primary = if let Some(call) = &prepared {
        call.op()
    } else if let Command::Auth(AuthCommand::Check { for_operation: Some(name) }) = &cli.command {
        op_ref(registry, std::slice::from_ref(name))?.0.op
    } else {
        match &cli.command {
            Command::Asset(AssetCommand::Upload(args)) => flows::upload::primary(registry, args),
            Command::Asset(AssetCommand::Download(args)) => flows::download::primary(registry, args),
            Command::Place(PlaceCommand::Publish(_)) => flows::op(registry, flows::PUBLISH),
            Command::Luau(LuauCommand::Run(args)) => flows::luau::primary(registry, args),
            _ => registry.op("assets.get-asset").expect("asset read is registered"),
        }
    };
    let force_key = matches!(cli.command, Command::Auth(AuthCommand::Check { .. }));
    let source = if force_key || matches!(primary.auth, Auth::ApiKey { .. }) {
        Some(
            auth::choose(
                &config,
                cli.profile.as_deref(),
                std::env::var("UPDRAFT_PROFILE").ok().as_deref(),
                std::env::var_os("UPDRAFT_API_KEY").is_some(),
            )
            .map_err(|message| auth::auth_error(primary, None, &config, message))?,
        )
    } else {
        None
    };
    let key = source
        .as_ref()
        .map(|source| {
            auth::load(&config, source).map_err(|message| auth::auth_error(primary, Some(source), &config, message))
        })
        .transpose()?;
    if let Some(key) = &key {
        output.redact_also(key);
    }
    let executor = Executor::new(
        registry,
        std::env::var("UPDRAFT_ORIGIN").ok(),
        key.as_ref(),
        Timing {
            deadline: Duration::from_secs_f64(cli.deadline),
            poll_interval: Duration::from_secs_f64(cli.poll_interval),
            retry_delay: cli.retry_delay.map(Duration::from_secs_f64),
        },
        output,
    )?;
    let profile = source.as_ref().and_then(|source| match source {
        auth::KeySource::Profile(name) => config.profiles.get(name),
        _ => None,
    });
    let result = (|| match cli.command {
        Command::Asset(AssetCommand::Upload(args)) => flows::upload::run(&executor, args, profile),
        Command::Asset(AssetCommand::Download(args)) => flows::download::run(&executor, args),
        Command::Place(PlaceCommand::Publish(args)) => flows::publish::run(&executor, args),
        Command::Luau(LuauCommand::Run(args)) => flows::luau::run(&executor, args),
        Command::Call(args) => {
            let call = prepared.expect("call arguments were validated");
            if args.all {
                executor.send_all(call, args.max_items)
            } else {
                let reply = executor.send(&call)?;
                if !args.no_wait && call.op().completion.is_some() {
                    executor.finish(call.op(), reply.json()?)
                } else {
                    reply.json()
                }
            }
        }
        Command::Auth(AuthCommand::Check { for_operation }) => {
            let introspect = registry.by_key("POST /api-keys/v1/introspect").expect("introspection is registered");
            let secret = key.as_ref().expect("auth check selects a key");
            let body = BodyInput::Json {
                content_type: "application/json".into(),
                bytes: serde_json::to_vec(&json!({"apiKey":secret.expose()})).expect("a JSON value serializes"),
            };
            let reply = executor.send(&Call::new(introspect, vec![], vec![], Some(body))?)?.json()?;
            let reply = auth::Introspection::parse(reply)?;
            let scopes = reply.scopes();
            if !reply.enabled || reply.expired {
                return Err(auth::auth_error(primary, source.as_ref(), &config, "the key is disabled or expired"));
            }
            if for_operation.is_some()
                && let Auth::ApiKey { scopes: Some(required) } = &primary.auth
            {
                let missing: Vec<_> = required
                    .iter()
                    .filter(|scope| !scopes.contains(&scope.name))
                    .map(|scope| scope.name.as_str())
                    .collect();
                if !missing.is_empty() {
                    return Err(auth::auth_error(
                        primary,
                        source.as_ref(),
                        &config,
                        format!("the selected key lacks {}", missing.join(", ")),
                    ));
                }
            }
            Ok(
                json!({"keySource":source.as_ref().map(auth::KeySource::label),"name":reply.name,"enabled":reply.enabled,"expired":reply.expired,"scopes":scopes}),
            )
        }
        _ => unreachable!("discovery and profiles returned before execution"),
    })();
    result.map_err(|failure| {
        if failure.code == 3 {
            failure
                .with("keySource", json!(source.as_ref().map(auth::KeySource::label)))
                .with("profiles", json!(config.profiles.keys().collect::<Vec<_>>()))
        } else {
            failure
        }
    })
}
fn op_ref<'r>(registry: &'r Registry, words: &[String]) -> Result<(Resolved<'r>, usize), Failure> {
    let first = words.first().ok_or_else(|| Failure::usage("an operation name or METHOD and URL is required"))?;
    let method = match first.as_str() {
        "GET" => Some(Method::Get),
        "POST" => Some(Method::Post),
        "PUT" => Some(Method::Put),
        "PATCH" => Some(Method::Patch),
        "DELETE" => Some(Method::Delete),
        _ => None,
    };
    let (reference, used) = if let Some(method) = method {
        let text = words.get(1).ok_or_else(|| Failure::usage("METHOD requires a URL path"))?;
        let url = url::Url::parse(&if text.starts_with('/') {
            format!("https://apis.roblox.com{text}")
        } else {
            text.clone()
        })
        .map_err(|_| Failure::usage("invalid URL"))?;
        (
            OpRef::Url {
                method,
                path: url.path().into(),
                query: url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect(),
            },
            2,
        )
    } else {
        (OpRef::Name(first.clone()), 1)
    };
    let mut resolved = registry.resolve(&reference).map_err(|error| match error {
        updraft_registry::ResolveError::Unknown { suggestions } => {
            Failure::usage(format!("unknown operation {}", words[..used].join(" ")))
                .with("suggestions", json!(suggestions))
        }
        updraft_registry::ResolveError::Ambiguous { names } => {
            Failure::usage("ambiguous operation").with("suggestions", json!(names))
        }
    })?;
    if matches!(reference, OpRef::Url { .. }) {
        for (name, value) in &mut resolved.preset {
            if resolved.op.params.iter().any(|p| p.location == updraft_registry::Location::Path && p.name == *name) {
                *value = percent_encoding::percent_decode_str(value).decode_utf8_lossy().into_owned();
            }
        }
    }
    Ok((resolved, used))
}
