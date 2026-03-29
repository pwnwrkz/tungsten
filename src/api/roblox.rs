use serde::{Deserialize, Serialize};

/// Request body sent when uploading an asset.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadRequest {
    pub asset_type: String,
    pub display_name: String,
    pub description: String,
    pub creation_context: CreationContext,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationContext {
    pub creator: Creator,
}

#[derive(Serialize, Clone)]
#[serde(untagged)]
pub enum Creator {
    User(UserCreator),
    Group(GroupCreator),
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UserCreator {
    pub user_id: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GroupCreator {
    pub group_id: String,
}

/// Response from polling an upload operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub done: bool,
    pub operation_id: String,
    pub response: Option<OperationResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationResult {
    pub asset_id: String,
}
