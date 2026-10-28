use clap::{Args, Parser, Subcommand};
#[derive(Parser)]
#[command(name = "updraft", version, about = "Roblox Open Cloud. One JSON document per command.", after_help = AGENT_GUIDE)]
pub struct Cli {
    /// Select the credential profile to use.
    #[arg(long, global = true)]
    pub profile: Option<String>,
    /// Stop polling after this many seconds.
    #[arg(long,global=true,default_value_t=300.0,value_parser=seconds)]
    pub deadline: f64,
    /// Wait this many seconds between polls.
    #[arg(long,global=true,default_value_t=2.0,value_parser=seconds)]
    pub poll_interval: f64,
    /// Override retry delays in seconds.
    #[arg(long,global=true,value_parser=seconds)]
    pub retry_delay: Option<f64>,
    /// Write request and response details to stderr.
    #[arg(long, global = true)]
    pub verbose: bool,
    #[command(subcommand)]
    pub command: Command,
}
fn seconds(s: &str) -> Result<f64, String> {
    s.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 86400.0)
        .ok_or_else(|| "seconds must be between 0 and 86400".into())
}
#[derive(Subcommand)]
pub enum Command {
    /// Find operations by words from their names and documentation.
    Find {
        /// Search words from operation names, paths, and documentation.
        #[arg(required = true)]
        query: Vec<String>,
        /// Return at most this many operations.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Show an operation contract and a ready-to-fill call.
    Describe {
        /// An operation name, or an HTTP method and URL.
        #[arg(required=true,num_args=1..=2)]
        operation: Vec<String>,
    },
    /// Call an operation by name or HTTP method and URL.
    Call(CallArgs),
    /// Upload and download assets.
    #[command(subcommand)]
    Asset(AssetCommand),
    /// Publish place files.
    #[command(subcommand)]
    Place(PlaceCommand),
    /// Run Luau and collect its results and logs.
    #[command(subcommand)]
    Luau(LuauCommand),
    /// Inspect credentials without printing their values.
    #[command(subcommand)]
    Auth(AuthCommand),
}
#[derive(Args)]
pub struct CallArgs {
    /// An operation name or method and URL, then name=value parameters.
    #[arg(required=true,num_args=1..)]
    pub words: Vec<String>,
    /// Send inline JSON, @file bytes, or @- from stdin.
    #[arg(long)]
    pub body: Option<String>,
    /// Add a multipart field as name=value or name=@file.
    #[arg(long = "part")]
    pub parts: Vec<String>,
    /// Follow pages and return one combined page document.
    #[arg(long)]
    pub all: bool,
    /// Stop after this many items and retain the continuation token.
    #[arg(long, requires = "all")]
    pub max_items: Option<usize>,
    /// Stop after this many pages and retain the continuation token.
    #[arg(long, requires = "all", default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..))]
    pub max_pages: u32,
    /// Return the first response without polling for completion.
    #[arg(long)]
    pub no_wait: bool,
}
#[derive(Subcommand)]
pub enum AuthCommand {
    /// Check the selected key and optionally its required scopes.
    Check {
        /// Check that the key has the scopes required by this operation.
        #[arg(long = "for")]
        for_operation: Option<String>,
    },
    /// List configured credential sources and key availability.
    Profiles,
}

#[derive(Subcommand)]
pub enum AssetCommand {
    /// Upload one asset file, then poll for its asset ID.
    Upload(crate::flows::upload::Args),
    /// Download an asset through a request that carries no API key.
    Download(crate::flows::download::Args),
}
#[derive(Subcommand)]
pub enum PlaceCommand {
    /// Publish a place file with the guarded retry policy.
    Publish(crate::flows::publish::Args),
}
#[derive(Subcommand)]
pub enum LuauCommand {
    /// Create a Luau task once, then poll and collect logs.
    Run(crate::flows::luau::Args),
}

const AGENT_GUIDE: &str = "Agent guide:
  find <words> -> describe <name> -> call <name> name=value
  Example: find data store entry; describe data-stores.get-entry
           call data-stores.get-entry universe_id=1 data_store_id=Profiles entry_id=player
  asset upload <file>       Create once, then poll for the asset ID.
  place publish <file>      Publish with the guarded retry policy.
  asset download <id>       Download to --out without sending the key to the CDN.
  luau run <file>           Create once, then collect results and logs.
  Keys: --profile > UPDRAFT_PROFILE > UPDRAFT_API_KEY > default_profile.
  Exit 0  Success. Read the document.
  Exit 2  usage. Fix invalid arguments, input, files, or read-result limits.
  Exit 3  auth. Missing key, HTTP 401/403, or failed key check. Fix the key or profile.
  Exit 4  rejected. Fix another 4xx request before repeating it.
  Exit 5  outcome_unknown. Check the remote resource before repeating a write.
  Exit 6  remote_failure. Inspect the operation error or Luau logs.
  Exit 7  unavailable. Unprocessed, unavailable, or unreadable response. Retry or resume a read.
  Exit 8  pending. Accepted work unfinished, or later poll or receipt failed. Run error.resume.";

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn undocumented(command: &clap::Command, path: &str) -> Vec<String> {
        let mut missing = Vec::new();
        if command.get_about().is_none_or(|about| about.to_string().trim().is_empty()) {
            missing.push(path.to_owned());
        }
        for argument in command.get_arguments() {
            if argument.get_help().is_none_or(|help| help.to_string().trim().is_empty()) {
                missing.push(format!("{path} {}", argument.get_id()));
            }
        }
        for child in command.get_subcommands() {
            missing.extend(undocumented(child, &format!("{path} {}", child.get_name())));
        }
        missing
    }

    #[test]
    fn every_command_and_argument_has_help() {
        let mut command = Cli::command();
        command.build();
        let missing = undocumented(&command, "updraft");
        assert!(missing.is_empty(), "missing help descriptions: {missing:?}");
        let mut broken =
            Cli::command().mut_subcommand("call", |call| call.mut_arg("body", |argument| argument.help("")));
        broken.build();
        assert!(undocumented(&broken, "updraft").iter().any(|path| path == "updraft call body"));
    }

    #[test]
    fn top_level_help_teaches_the_agent_loop_and_exit_codes() {
        let help = Cli::command().render_long_help().to_string();
        for text in [
            "find <words>",
            "describe <name>",
            "call <name>",
            "data-stores.get-entry",
            "asset upload",
            "place publish",
            "asset download",
            "luau run",
            "UPDRAFT_API_KEY",
        ] {
            assert!(help.contains(text), "agent help must include {text}");
        }
        for exit in [0, 2, 3, 4, 5, 6, 7, 8] {
            assert!(help.contains(&format!("Exit {exit} ")), "agent help documents exit {exit}");
        }
    }

    #[test]
    fn agent_guide_aligns_flow_descriptions() {
        let help = Cli::command().render_long_help().to_string();
        let columns: Vec<_> = help
            .lines()
            .filter(|line| {
                ["asset upload <file>", "place publish <file>", "asset download <id>", "luau run <file>"]
                    .iter()
                    .any(|command| line.trim_start().starts_with(command))
            })
            .map(|line| {
                line.find("> ").unwrap()
                    + 2
                    + line.split_once("> ").unwrap().1.chars().take_while(|character| *character == ' ').count()
            })
            .collect();
        assert_eq!(columns.len(), 4);
        assert!(columns.iter().all(|column| *column == columns[0]), "flow descriptions start at {columns:?}");
    }
}
