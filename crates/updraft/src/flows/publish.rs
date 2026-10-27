use crate::call::Call;
use crate::exec::Executor;
use crate::outcome::Failure;
use serde_json::{Value, json};
use std::path::PathBuf;
#[derive(clap::Args)]
pub struct Args {
    pub file: PathBuf,
    #[arg(long,value_parser=super::positive_id)]
    pub universe: String,
    #[arg(long,value_parser=super::positive_id)]
    pub place: String,
    #[arg(long, value_enum, default_value = "published")]
    pub version_type: VersionType,
}
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum VersionType {
    Published,
    Saved,
}
pub fn run(ex: &Executor<'_>, args: Args) -> Result<Value, Failure> {
    let bytes = super::file(&args.file, None)?;
    let op = super::op(ex.registry, super::PUBLISH);
    let Some(updraft_registry::Body { media: updraft_registry::BodyMedia::Binary { content_type }, .. }) = &op.body
    else {
        unreachable!("publish overlay declares a binary body")
    };
    let version = match args.version_type {
        VersionType::Published => "Published",
        VersionType::Saved => "Saved",
    };
    let call = Call::new(
        op,
        vec![],
        vec![
            ("universeId".into(), args.universe.clone()),
            ("placeId".into(), args.place.clone()),
            ("versionType".into(), version.into()),
        ],
        Some(crate::call::BodyInput::Binary { content_type: content_type.clone(), bytes }),
    )?;
    let reply = ex.send(&call)?.json()?;
    let version_number = reply["versionNumber"]
        .as_u64()
        .ok_or_else(|| Failure::unknown("publish response has no valid versionNumber"))?;
    Ok(
        json!({"universeId":args.universe,"placeId":args.place,"versionNumber":version_number,"versionType":version.to_ascii_lowercase()}),
    )
}
