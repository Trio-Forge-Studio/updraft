use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
#[derive(Parser)]
#[command(name = "updraft", version, about = "Roblox Open Cloud. One JSON document per command.")]
pub struct Cli {
    #[arg(long, global = true)]
    pub profile: Option<String>,
    #[arg(long,global=true,default_value_t=300.0,value_parser=seconds)]
    pub deadline: f64,
    #[arg(long,global=true,default_value_t=2.0,value_parser=seconds)]
    pub poll_interval: f64,
    #[arg(long,global=true,value_parser=seconds)]
    pub retry_delay: Option<f64>,
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
    Find {
        #[arg(required = true)]
        query: Vec<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    Describe {
        #[arg(required=true,num_args=1..=2)]
        operation: Vec<String>,
    },
    Call(CallArgs),
    #[command(subcommand)]
    Auth(AuthCommand),
}
#[derive(Args)]
pub struct CallArgs {
    #[arg(required=true,num_args=1..)]
    pub words: Vec<String>,
    #[arg(long)]
    pub body: Option<String>,
    #[arg(long = "part")]
    pub parts: Vec<String>,
    #[arg(long)]
    pub all: bool,
    #[arg(long, requires = "all")]
    pub max_items: Option<usize>,
    #[arg(long)]
    pub no_wait: bool,
    #[arg(long)]
    pub out: Option<PathBuf>,
}
#[derive(Subcommand)]
pub enum AuthCommand {
    Check {
        #[arg(long = "for")]
        for_operation: Option<String>,
    },
    Profiles,
}
