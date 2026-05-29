use spacetimedb::Timestamp;

#[spacetimedb::table(accessor = session, public)]
pub struct Session {
    #[primary_key]
    pub id: String,
    pub base_name: String,
    pub host: String,
    pub client_id: String,
    pub created_at: Timestamp,
    pub last_seen: Timestamp,
    pub context_used: u64,
    pub context_window: u64,
}

#[spacetimedb::table(accessor = session_activity, public)]
pub struct SessionActivity {
    #[primary_key]
    pub session_id: String,
    pub state: String,
    pub last_tool_event: Option<String>,
    pub updated_at: Timestamp,
}

#[spacetimedb::table(accessor = message, public)]
pub struct Message {
    #[primary_key]
    pub id: String,
    #[index(btree)]
    pub session_id: String,
    pub role: String,
    pub text: String,
    pub source: String,
    #[index(btree)]
    pub created_at: Timestamp,
}

#[spacetimedb::table(accessor = message_image, public)]
pub struct MessageImage {
    #[primary_key]
    pub message_id: String,
    pub bytes: Vec<u8>,
}

#[spacetimedb::table(accessor = tool_event, public)]
pub struct ToolEvent {
    #[primary_key]
    pub id: String,
    #[index(btree)]
    pub session_id: String,
    pub tool: String,
    pub detail: String,
    pub started_at: Timestamp,
}

#[spacetimedb::table(accessor = permission_request, public)]
pub struct PermissionRequest {
    #[primary_key]
    pub id: String,
    #[index(btree)]
    pub session_id: String,
    pub tool: String,
    pub input: String,
    pub status: String,
    pub created_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
}
