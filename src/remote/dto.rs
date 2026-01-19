use serde::Deserialize;
use serde::Serialize;

use std::collections::HashMap;

#[derive(Serialize, Debug)]
pub struct BatchRequest {
	pub operation: String,
	pub transfers: Vec<String>,
	pub objects: Vec<BatchObject>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub hash_algo: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct BatchObject {
	pub oid: String,
	pub size: u64,
}

#[derive(Deserialize, Debug)]
pub struct BatchResponse {
	pub transfer: Option<String>,
	pub objects: Vec<BatchResponseObject>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub hash_algo: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct BatchResponseObject {
	pub oid: String,
	pub size: u64,
	pub authenticated: Option<bool>,
	pub actions: Option<ObjectActions>,
	pub error: Option<ObjectError>,
}

#[derive(Deserialize, Debug)]
pub struct ObjectActions {
	pub download: Option<ObjectAction>,
	pub upload: Option<ObjectAction>,
	pub verify: Option<ObjectAction>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ObjectAction {
	pub href: String,
	#[serde(default)]
	pub header: HashMap<String, String>,
	pub expires_in: Option<u64>,
	pub expires_at: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct ObjectError {
	pub code: u32,
	pub message: String,
}

#[derive(Serialize, Debug)]
pub struct LockRequest {
	pub path: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub ref_name: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct LockResponse {
	pub lock: Lock,
	pub message: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Lock {
	pub id: String,
	pub path: String,
	pub locked_at: String,
	pub owner: LockOwner,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LockOwner {
	pub name: String,
}

#[derive(Deserialize, Debug)]
pub struct LockListResponse {
	pub locks: Vec<Lock>,
	pub next_cursor: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct UnlockRequest {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub force: Option<bool>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub ref_name: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct UnlockResponse {
	pub lock: Lock,
	pub message: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct VerifyLocksResponse {
	pub ours: Vec<Lock>,
	pub theirs: Vec<Lock>,
	pub next_cursor: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct ErrorResponse {
	pub message: String,
	pub documentation_url: Option<String>,
	pub request_id: Option<String>,
}
