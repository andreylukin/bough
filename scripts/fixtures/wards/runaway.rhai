// Exceeds `max_ops`: the host TERMINATES it, names it, and does not retry it into a loop.
fn on_event(ev, cx) {
    let n = 0;
    loop { n += 1; }
    []
}
