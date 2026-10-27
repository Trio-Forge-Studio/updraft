pub mod download;
pub mod luau;
pub mod publish;
pub mod upload;
use crate::outcome::Failure;
use std::path::Path;
use updraft_registry::{Op, Registry};
pub const CREATE_ASSET: &str = "POST /assets/v1/assets";
pub const GET_OPERATION: &str = "GET /assets/v1/operations/{operationId}";
pub const PUBLISH: &str = "POST /universes/v1/{universeId}/places/{placeId}/versions";
pub const DELIVERY: &str = "GET /asset-delivery-api/v1/assetId/{assetId}";
pub const DELIVERY_VERSION: &str = "GET /asset-delivery-api/v1/assetId/{assetId}/version/{versionNumber}";
pub const LUAU: &str = "POST /cloud/v2/universes/{universe_id}/places/{place_id}/luau-execution-session-tasks";
pub const LUAU_VERSION: &str =
    "POST /cloud/v2/universes/{universe_id}/places/{place_id}/versions/{version_id}/luau-execution-session-tasks";
pub fn op<'r>(r: &'r Registry, key: &str) -> &'r Op {
    r.by_key(key).expect("flow key is in the checked-in registry")
}
pub fn positive_id(s: &str) -> Result<String, String> {
    if s.starts_with(['1', '2', '3', '4', '5', '6', '7', '8', '9']) && s.bytes().all(|b| b.is_ascii_digit()) {
        Ok(s.into())
    } else {
        Err("expected a positive decimal ID".into())
    }
}
pub fn file(path: &Path, maximum: Option<u64>) -> Result<Vec<u8>, Failure> {
    let metadata = std::fs::metadata(path).map_err(|_| Failure::usage("cannot read the input file"))?;
    if !metadata.is_file() || metadata.len() == 0 || maximum.is_some_and(|max| metadata.len() > max) {
        return Err(Failure::usage("input must be a nonempty regular file within the size limit"));
    }
    let bytes = std::fs::read(path).map_err(|_| Failure::usage("cannot read the input file"))?;
    if bytes.is_empty() || maximum.is_some_and(|max| bytes.len() as u64 > max) {
        return Err(Failure::usage("input changed to an invalid size"));
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    #[test]
    fn flow_keys_resolve() {
        let r = &*crate::REGISTRY;
        for key in [
            super::CREATE_ASSET,
            super::GET_OPERATION,
            super::PUBLISH,
            super::DELIVERY,
            super::DELIVERY_VERSION,
            super::LUAU,
            super::LUAU_VERSION,
        ] {
            assert!(r.by_key(key).is_some(), "flow key {key} must exist");
        }
    }
}
