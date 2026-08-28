// PLANTED: a type that declares Emit, dispatched as a waterfall.
pub struct Ping;
impl EmitEvent for Ping {
    const NAME: &'static str = "fixture/ping";
    type Payload = ();
}
fn dispatch(ctx: &Context) {
    let _ = ctx.waterfall::<Ping>(());
}
