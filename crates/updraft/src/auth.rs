use crate::outcome::Failure;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use updraft_registry::Op;

pub struct Secret(String);
#[derive(Deserialize)]
pub struct Introspection {
    pub name: String,
    pub enabled: bool,
    pub expired: bool,
    scopes: Vec<IntrospectedScope>,
}
#[derive(Deserialize)]
struct IntrospectedScope {
    name: String,
    operations: Vec<String>,
}
impl Introspection {
    pub fn parse(value: serde_json::Value) -> Result<Self, Failure> {
        serde_json::from_value(value).map_err(|_| Failure::unavailable("introspection returned an invalid response"))
    }
    pub fn scopes(&self) -> Vec<String> {
        let mut scopes: Vec<_> = self
            .scopes
            .iter()
            .flat_map(|scope| scope.operations.iter().map(|op| format!("{}:{op}", scope.name)))
            .collect();
        scopes.sort();
        scopes.dedup();
        scopes
    }
}
impl Secret {
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
    pub(crate) fn from_string(value: String) -> Self {
        Self(value)
    }
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub default_profile: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub key_env: Option<String>,
    pub key_file: Option<PathBuf>,
    pub group: Option<u64>,
    pub user: Option<u64>,
}
#[derive(Debug, PartialEq)]
pub enum KeySource {
    Profile(String),
    Environment,
}
impl KeySource {
    pub fn label(&self) -> String {
        match self {
            Self::Profile(name) => format!("profile {name}"),
            Self::Environment => "UPDRAFT_API_KEY".into(),
        }
    }
}
pub fn scope_metadata(op: &Op) -> serde_json::Value {
    let scopes = op.auth.required_scopes();
    json!({"requiredScopes":scopes,"scopeMetadata":if scopes.is_some() { "declared" } else { "unknown" }})
}
pub fn decorate(mut failure: Failure, op: &Op, source: Option<&KeySource>, config: &Config) -> Failure {
    if failure.code == 3 {
        let error = failure.error.as_object_mut().expect("a failure is an object");
        error.entry("status").or_insert(serde_json::Value::Null);
        error.extend(scope_metadata(op).as_object().expect("scope metadata is an object").clone());
        failure = failure
            .with("keySource", json!(source.map(KeySource::label)))
            .with("profiles", json!(config.profiles.keys().collect::<Vec<_>>()));
    }
    failure
}

pub fn choose(
    config: &Config,
    explicit: Option<&str>,
    env_profile: Option<&str>,
    env_key_present: bool,
) -> Result<KeySource, String> {
    if let Some(name) = explicit.or(env_profile) {
        if !config.profiles.contains_key(name) {
            return Err(format!("profile {name} does not exist"));
        }
        return Ok(KeySource::Profile(name.into()));
    }
    if env_key_present {
        return Ok(KeySource::Environment);
    }
    if let Some(name) = &config.default_profile {
        if !config.profiles.contains_key(name) {
            return Err(format!("default profile {name} does not exist"));
        }
        return Ok(KeySource::Profile(name.clone()));
    }
    Err("set UPDRAFT_API_KEY or select a configured profile".into())
}
pub fn load(config: &Config, source: &KeySource) -> Result<Secret, String> {
    let value = match source {
        KeySource::Environment => {
            std::env::var("UPDRAFT_API_KEY").map_err(|_| "UPDRAFT_API_KEY is missing".to_string())?
        }
        KeySource::Profile(name) => {
            let p = config.profiles.get(name).ok_or_else(|| format!("profile {name} does not exist"))?;
            match (&p.key_env, &p.key_file) {
                (Some(var), None) => {
                    std::env::var(var).map_err(|_| format!("profile {name} key environment variable is missing"))?
                }
                (None, Some(path)) => {
                    std::fs::read_to_string(path).map_err(|_| format!("profile {name} key file cannot be read"))?
                }
                _ => return Err(format!("profile {name} must have exactly one of key_env and key_file")),
            }
        }
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{} is empty", source.label()));
    }
    Ok(Secret::from_string(value.into()))
}
pub fn load_config() -> Result<Config, Failure> {
    let path = std::env::var_os("UPDRAFT_CONFIG")
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|dir| dir.join("updraft/config.toml")))
        .ok_or_else(|| Failure::usage("cannot locate the config directory"))?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(_) => return Err(Failure::usage("cannot read the updraft config file")),
    };
    toml::from_str(&text).map_err(|_| {
        Failure::usage("invalid updraft config, expected default_profile and profiles with key_env or key_file")
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn precedence_and_no_fallthrough() {
        let c: Config =
            toml::from_str("default_profile='a'\n[profiles.a]\nkey_env='A'\n[profiles.b]\nkey_env='B'").unwrap();
        assert_eq!(choose(&c, Some("a"), Some("b"), true).unwrap(), KeySource::Profile("a".into()));
        assert_eq!(choose(&c, None, Some("b"), true).unwrap(), KeySource::Profile("b".into()));
        assert_eq!(choose(&c, None, None, true).unwrap(), KeySource::Environment);
        assert_eq!(choose(&c, None, None, false).unwrap(), KeySource::Profile("a".into()));
        assert!(choose(&c, Some("missing"), None, true).is_err());
    }
}
