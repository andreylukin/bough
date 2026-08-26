// Every kind a ward may return, so the vocabulary has a fixture that exercises it.
fn triggers() { ["thought/text"] }

fn on_event(ev, cx) {
    [
        #{ kind: "mark", agent: "sol", mark: "claim", text: "the ward saw a thought",
           cites: ["step:" + ev.seq] },
        #{ kind: "act", action_kind: "open_pr", target: "owner/repo", payload: #{} },
    ]
}
