// A clean fixture: one declaration per trait, each dispatched under the mode it declares.
pub struct Ping;
impl EmitEvent for Ping {
    const NAME: &'static str = "fixture/ping";
    type Payload = ();
}
fn dispatch(ctx: &Context) {
    ctx.emit::<Ping>(());
}
