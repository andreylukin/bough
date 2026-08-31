// A ward is PURE: `on_event` RETURNS actions and performs none.
// The host is what executes them, through the seams, under its own limits.

fn triggers() { ["mail/delivered"] }

fn on_event(ev, cx) {
    if ev.body.subject == "" { return []; }
    [ #{ kind: "hint", agent: cx.ward, text: "a review request landed: " + ev.body.subject } ]
}
