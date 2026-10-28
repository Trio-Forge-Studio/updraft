use crate::call::Call;
use crate::exec::Executor;
use crate::outcome::Failure;
use serde_json::{Value, json};
use std::path::PathBuf;
#[derive(clap::Args)]
pub struct Args {
    /// The asset ID to download.
    #[arg(value_parser=super::positive_id)]
    pub asset: String,
    /// Write the downloaded bytes to this file.
    #[arg(long)]
    pub out: PathBuf,
    /// Download this asset version instead of the latest.
    #[arg(long)]
    pub version: Option<u32>,
    /// Replace an existing output file after a successful download.
    #[arg(long)]
    pub overwrite: bool,
}
pub fn primary<'r>(r: &'r updraft_registry::Registry, args: &Args) -> &'r updraft_registry::Op {
    super::op(r, if args.version.is_some() { super::DELIVERY_VERSION } else { super::DELIVERY })
}
pub fn run(ex: &Executor<'_>, args: Args) -> Result<Value, Failure> {
    if args.out.exists() && !args.overwrite {
        return Err(Failure::usage("output already exists; use --overwrite to replace it"));
    }
    let mut params = vec![("assetId".into(), args.asset.clone())];
    if let Some(version) = args.version {
        params.push(("versionNumber".into(), version.to_string()));
    }
    let call = Call::new(primary(ex.registry, &args), vec![], params, None)?;
    let reply = ex.send(&call)?.json()?;
    let location =
        reply["location"].as_str().ok_or_else(|| Failure::unavailable("delivery returned no download location"))?;
    let bytes = ex.download(location, &args.out, args.overwrite)?;
    Ok(json!({"assetId":args.asset,"version":args.version,"file":args.out,"bytes":bytes}))
}
