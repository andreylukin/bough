//! Invariant: an agent is identified by its `AgentId` (this life) and named by its `AgentName`
//! (the ledger row). The two are never interchanged: a resume gives a NEW `AgentId` under the
//! SAME `AgentName`.

bough_util::brand_id!(
    /// One live agent handle. Fresh on every create AND on every resume.
    pub struct AgentId;
);
bough_util::brand_id!(
    /// One private session: an agent's own view of its trajectory.
    pub struct SessionId;
);
bough_util::brand_id!(
    /// One message. Identifies its insertion, its claim and its discard (§2), so an inbox
    /// mutation is idempotent to replay.
    pub struct MessageId;
);
bough_util::brand_id!(
    /// One worker run.
    ///
    /// Declared here rather than in `plugins/workers` (which is where §10 spells it) because
    /// `Sender::Worker` names it and `workers` depends on `agents`. `bough-plugin-workers`
    /// re-exports it, so §10's spelling is unchanged for its readers.
    pub struct WorkerId;
);

macro_rules! id_json_schema {
    ($($t:ty),* $(,)?) => {$(
        impl schemars::JsonSchema for $t {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($t))
            }
            fn json_schema(_g: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({ "type": "string" })
            }
        }
    )*};
}

id_json_schema!(AgentId, SessionId, MessageId, WorkerId);
