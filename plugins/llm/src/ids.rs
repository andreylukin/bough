//! Invariant: the model-facing tool identifiers are branded here, not in `tools`, because the
//! chunk vocabulary names them and `tools` depends on this crate for `LlmToolDef` (a cycle
//! otherwise). `plugins/tools` re-exports both, so §9's spelling is unchanged for its readers.

bough_util::brand_id!(
    /// The model-visible name of a tool, e.g. `bash`.
    pub struct ToolName;
);
bough_util::brand_id!(
    /// One tool call within one step; pairs a `tool/call` with its `tool/result` (§9).
    pub struct ToolCallId;
);
bough_util::brand_id!(
    /// The catalog-facing name of an adapter, e.g. `llm-anthropic`.
    pub struct AdapterName;
);

/// A branded id is a plain string in a body schema. `brand_id!` lives in `bough-util`, which has
/// no `schemars` dependency, so the impls are written here — the same arrangement `plugins/ledger`
/// uses, for the same reason.
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

id_json_schema!(ToolName, ToolCallId, AdapterName);
