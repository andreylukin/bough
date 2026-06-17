//// HTTP + SSE routes, published as an OpenAPI 3.1 spec at `/doc` so clients and
//// SDKs can be generated (opencode-style, SPEC.md §8). Reserved shape only;
//// implemented once the transport (SSE vs WebSocket) is chosen (SPEC.md §11).

/// The route surface bough intends to expose.
pub type Route {
  /// GET /doc — OpenAPI 3.1 spec.
  Doc
  /// POST /session — create a session.
  CreateSession
  /// POST /session/:id/message — submit a prompt; streams over SSE.
  SendMessage
  /// POST /session/:id/fork — fork from a node (restores its snapshot).
  Fork
  /// GET /session/:id/events — SSE stream: tokens, tool events, net audit.
  Events
}
