use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct Request {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub result: Value,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub error: ErrorObject,
}

#[derive(Debug, Serialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
}
