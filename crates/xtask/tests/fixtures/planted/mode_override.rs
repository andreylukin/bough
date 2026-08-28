// PLANTED: an `EmitEvent` impl whose `const MODE` says Serial. The catalog surface and the
// dispatcher would disagree, and the compiler cannot see it.
pub struct Ping;
impl EmitEvent for Ping {
    const NAME: &'static str = "fixture/ping";
    const MODE: DispatchMode = DispatchMode::Serial;
    type Payload = ();
}
