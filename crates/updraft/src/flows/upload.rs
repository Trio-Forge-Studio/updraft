use crate::auth::Profile;
use crate::call::{BodyInput, Call, PartContent, PartInput};
use crate::exec::Executor;
use crate::outcome::Failure;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// The asset file to upload.
    #[arg(required_unless_present = "resume", conflicts_with = "resume")]
    pub file: Option<PathBuf>,
    /// Resume an accepted upload by operation ID using GET only.
    #[arg(long,value_parser=operation_id,conflicts_with_all=["file","asset_type","name","description","group","user"])]
    pub resume: Option<String>,
    /// Override the asset type inferred from the file extension.
    #[arg(long = "type", value_enum)]
    pub asset_type: Option<AssetType>,
    /// Override the display name inferred from the file stem.
    #[arg(long)]
    pub name: Option<String>,
    /// Set the asset description.
    #[arg(long, default_value = "")]
    pub description: String,
    /// Create the asset under this group ID.
    #[arg(long,value_parser=super::positive_id,conflicts_with="user")]
    pub group: Option<String>,
    /// Create the asset under this user ID.
    #[arg(long,value_parser=super::positive_id)]
    pub user: Option<String>,
}
#[derive(Clone, Copy, clap::ValueEnum, serde::Serialize)]
pub enum AssetType {
    Model,
    Decal,
    Audio,
    Video,
}
pub fn primary<'r>(r: &'r updraft_registry::Registry, args: &Args) -> &'r updraft_registry::Op {
    super::op(r, if args.resume.is_some() { super::GET_OPERATION } else { super::CREATE_ASSET })
}
pub fn operation_id(s: &str) -> Result<String, String> {
    let id = s.strip_prefix("operations/").unwrap_or(s);
    if !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        Ok(id.into())
    } else {
        Err("expected an operation ID or operations/<id>".into())
    }
}
enum Upload {
    Create { body: BodyInput, display_name: String, asset_type: AssetType },
    Resume(String),
}
fn prepare(args: Args, profile: Option<&Profile>) -> Result<Upload, Failure> {
    if let Some(id) = args.resume {
        return Ok(Upload::Resume(id));
    }
    let path = args.file.as_ref().expect("clap requires a file for creation");
    let (kind, media) = media(path)?;
    let kind = args.asset_type.unwrap_or(kind);
    let creator = match (args.group, args.user, profile) {
        (Some(id), _, _) => json!({"groupId":id}),
        (_, Some(id), _) => json!({"userId":id}),
        (_, _, Some(p)) if p.group.is_some_and(|id| id > 0) => json!({"groupId":p.group.unwrap().to_string()}),
        (_, _, Some(p)) if p.user.is_some_and(|id| id > 0) => json!({"userId":p.user.unwrap().to_string()}),
        _ => return Err(Failure::usage("set --group or --user, or configure the profile's group")),
    };
    let name = args.name.unwrap_or_else(|| title(path.file_stem().unwrap_or_default().to_string_lossy().as_ref()));
    let request = json!({"assetType":kind,"displayName":name,"description":args.description,"creationContext":{"creator":creator}});
    let body = BodyInput::Multipart {
        parts: vec![
            PartInput {
                name: "request".into(),
                content: PartContent::Json(serde_json::to_vec(&request).expect("a JSON value serializes")),
            },
            PartInput {
                name: "fileContent".into(),
                content: PartContent::File {
                    file_name: path.file_name().unwrap_or_default().to_string_lossy().into(),
                    content_type: media.into(),
                    bytes: super::file(path, Some(20_000_000))?,
                },
            },
        ],
    };
    Ok(Upload::Create { body, display_name: name, asset_type: kind })
}
fn create(ex: &Executor<'_>, body: BodyInput) -> Result<String, Failure> {
    let op = super::op(ex.registry, super::CREATE_ASSET);
    let first = ex.send(&Call::new(op, vec![], vec![], Some(body))?).and_then(|v| v.json()).map_err(upload_hint)?;
    if !first["error"].is_null() {
        return Err(Failure::remote("upload operation failed", first));
    }
    first["path"]
        .as_str()
        .or_else(|| first["operationId"].as_str())
        .and_then(|s| operation_id(s).ok())
        .ok_or_else(|| upload_hint(Failure::unknown("no valid operation ID")))
}
pub fn run(ex: &Executor<'_>, args: Args, profile: Option<&Profile>) -> Result<Value, Failure> {
    let (id, display_name, asset_type) = match prepare(args, profile)? {
        Upload::Resume(id) => (id, None, None),
        Upload::Create { body, display_name, asset_type } => (create(ex, body)?, Some(display_name), Some(asset_type)),
    };
    ex.note(&format!("Operation {id}: waiting"));
    let resume = format!("updraft asset upload --resume {id}");
    ex.resume(resume);
    let result = ex
        .finish(super::op(ex.registry, super::GET_OPERATION), json!({"path":format!("operations/{id}"),"done":false}))
        .map_err(|f| f.with("operationId", json!(id)))?;
    let asset = result["response"]["assetId"].as_str().and_then(|s| super::positive_id(s).ok()).ok_or_else(|| {
        Failure::unavailable("operation finished without a valid assetId, protocol failure")
            .with("detail", result.clone())
    })?;
    Ok(json!({"assetId":asset,"operationId":id,"displayName":display_name,"assetType":asset_type}))
}
fn upload_hint(f: Failure) -> Failure {
    if f.code == 5 {
        f.with("hint", json!("Check the group's Creations before uploading again. updraft never re-sends an upload."))
    } else {
        f
    }
}
pub(crate) fn media(path: &std::path::Path) -> Result<(AssetType, &'static str), Failure> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "fbx" => Ok((AssetType::Model, "model/fbx")),
        "gltf" => Ok((AssetType::Model, "model/gltf+json")),
        "glb" => Ok((AssetType::Model, "model/gltf-binary")),
        "rbxm" => Ok((AssetType::Model, "model/x-rbxm")),
        "png" => Ok((AssetType::Decal, "image/png")),
        "jpg" | "jpeg" => Ok((AssetType::Decal, "image/jpeg")),
        "bmp" => Ok((AssetType::Decal, "image/bmp")),
        "tga" => Ok((AssetType::Decal, "image/tga")),
        "ogg" => Ok((AssetType::Audio, "audio/ogg")),
        "mp3" => Ok((AssetType::Audio, "audio/mpeg")),
        "wav" => Ok((AssetType::Audio, "audio/wav")),
        "flac" => Ok((AssetType::Audio, "audio/flac")),
        "mp4" => Ok((AssetType::Video, "video/mp4")),
        _ => Err(Failure::usage("unsupported asset file extension")),
    }
}
fn title(stem: &str) -> String {
    stem.split(['-', '_', ' '])
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut chars = s.chars();
            chars.next().map(|c| c.to_uppercase().to_string() + chars.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}
