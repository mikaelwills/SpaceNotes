use spacetimedb::Timestamp;

#[spacetimedb::table(accessor = agent, public)]
pub struct Agent {
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

#[spacetimedb::table(accessor = agent_activity, public)]
pub struct AgentActivity {
    #[primary_key]
    pub agent_id: String,
    pub state: String,
    pub last_tool_event: Option<String>,
    pub updated_at: Timestamp,
}

#[spacetimedb::table(accessor = message, public)]
pub struct Message {
    #[primary_key]
    pub id: String,
    #[index(btree)]
    pub agent_id: String,
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
    pub agent_id: String,
    pub tool: String,
    pub detail: String,
    pub started_at: Timestamp,
}

#[spacetimedb::table(accessor = permission_request, public)]
pub struct PermissionRequest {
    #[primary_key]
    pub id: String,
    #[index(btree)]
    pub agent_id: String,
    pub tool: String,
    pub input: String,
    pub status: String,
    pub created_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
}

#[spacetimedb::table(accessor = question_request, public)]
pub struct QuestionRequest {
    #[primary_key]
    pub id: String,
    #[index(btree)]
    pub agent_id: String,
    pub question: String,
    pub header: String,
    /// JSON-encoded array of option labels.
    pub options: String,
    pub multi_select: bool,
    /// "pending" until answered, then "answered".
    pub status: String,
    /// JSON-encoded selected label(s); None until answered.
    pub response: Option<String>,
    pub created_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
}
