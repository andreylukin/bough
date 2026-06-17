//// Provider-agnostic LLM interface. Anthropic ships first (SPEC.md §5); other
//// providers implement the same `Provider` record.

pub type Message {
  Message(role: String, content: String)
}

pub type ToolCall {
  ToolCall(id: String, name: String, arguments: String)
}

pub type StopReason {
  EndTurn
  ToolUse
  MaxTokens
}

pub type Completion {
  Completion(text: String, tool_calls: List(ToolCall), stop: StopReason)
}

/// A provider is a record of capabilities rather than a typeclass, so the core
/// stays agnostic and providers can be swapped at runtime.
pub type Provider {
  Provider(
    name: String,
    complete: fn(List(Message), List(String)) -> Result(Completion, String),
  )
}
