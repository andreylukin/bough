use bough_core::harness::protocol::HostFnName as H;
use bough_core::prompt::assemble::{assemble_prompt, PromptInput};
use bough_core::schema::parts::SessionKind;
fn main() {
    // The REAL boot grant from bough-server/src/boot.rs, plus top-tier delegation.
    let g = vec![
        H::Bash,
        H::Sh,
        H::BashBg,
        H::BashOutput,
        H::BashWait,
        H::BashKill,
        H::View,
        H::Patch,
        H::Write,
        H::Workflow,
        H::Schedule,
        H::Ask,
        H::State,
        H::Artifact,
        H::Agent,
        H::Spawn,
        H::Join,
        H::Adopt,
    ];
    print!(
        "{}",
        assemble_prompt(&PromptInput::new(SessionKind::Root, g)).system
    );
}
