//! Thread operations (port of `src/history/{seed,fork,unsend,compact,extract,
//! move,handoff,sections,explore}.ts`). Sources stay byte-identical after
//! branching; unsend is the only destructive thread write.

pub mod compact;
pub mod explore;
pub mod extract;
pub mod fork;
pub mod handoff;
pub mod move_into;
pub mod sections;
pub mod seed;
pub mod unsend;
