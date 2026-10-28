use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    pub source: SpecSource,
    pub ops: Vec<Op>,
    pub schemas: BTreeMap<String, Shape>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecSource {
    pub repository: String,
    pub commit: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Op {
    pub name: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub method: Method,
    pub server: Server,
    pub path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<Param>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
    pub response: Payload,
    pub auth: Auth,
    pub stability: Stability,
    #[serde(default, skip_serializing_if = "is_false")]
    pub legacy: bool,
    pub safety: Safety,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagination: Option<Pagination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<Completion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryPolicy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<String>,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Server {
    Apis,
    Thumbnails,
}

impl Server {
    pub fn origin(self) -> &'static str {
        match self {
            Server::Apis => "https://apis.roblox.com",
            Server::Thumbnails => "https://thumbnails.roblox.com",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(rename = "in")]
    pub location: Location,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    pub kind: ValueKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Location {
    Path,
    Query,
    Header,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "type")]
pub enum ValueKind {
    String,
    Integer,
    Number,
    Boolean,
    Enum { values: Vec<String> },
    Array { items: Box<ValueKind> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Body {
    pub required: bool,
    pub media: BodyMedia,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "type")]
pub enum BodyMedia {
    Json { content_type: String, shape: Shape },
    Multipart { parts: Vec<Part> },
    Binary { content_type: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    pub kind: PartKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "type")]
pub enum PartKind {
    Json { shape: Shape },
    File,
    Text,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "type")]
pub enum Payload {
    Empty,
    Json { shape: Shape },
    Text,
    Binary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "type")]
pub enum Shape {
    Any,
    String {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
    },
    Integer,
    Number,
    Boolean,
    Enum {
        values: Vec<String>,
    },
    Array {
        items: Box<Shape>,
    },
    Map {
        values: Box<Shape>,
    },
    Object {
        fields: Vec<Field>,
    },
    OneOf {
        variants: Vec<Shape>,
    },
    Ref {
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,
    pub shape: Shape,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Auth {
    ApiKey { scopes: Option<Vec<Scope>> },
    BodyKey { field: String },
    None,
}

impl Auth {
    pub fn required_scopes(&self) -> Option<&[Scope]> {
        match self {
            Self::ApiKey { scopes } => scopes.as_deref(),
            Self::BodyKey { .. } | Self::None => Some(&[]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Scope {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stability {
    Stable,
    Beta,
    Experimental,
    Unspecified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Safety {
    Read,
    Idempotent,
    Unsafe,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pagination {
    pub token_param: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_param: Option<String>,
    pub next_field: String,
    pub items_field: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "type")]
pub enum Completion {
    Operation,
    State { field: String, success: Vec<String>, failure: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub statuses: Vec<StatusClass>,
    pub attempts: u8,
    pub delay_seconds: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StatusClass {
    Exact(u16),
    Class(ClassName),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClassName {
    #[serde(rename = "5xx")]
    ServerError,
}

impl StatusClass {
    pub fn matches(self, status: u16) -> bool {
        match self {
            StatusClass::Exact(code) => code == status,
            StatusClass::Class(ClassName::ServerError) => (500..600).contains(&status),
        }
    }
}
