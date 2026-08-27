// PLANTED: one event NAME declared twice, under two different dispatch modes.
pub struct PingA;
impl EmitEvent for PingA {
    const NAME: &'static str = "fixture/ping";
    type Payload = ();
}
pub struct PingB;
impl SerialEvent for PingB {
    const NAME: &'static str = "fixture/ping";
    type Payload = ();
}
