//! The tag-history memory (port of `src/history/{record,hygiene,stats,echo,
//! embed}.ts`): `bash(cmd, tags)` observations recorded per repo, hygiene
//! (snap, demote, drop only what FTS still finds), ACT-R popularity stats,
//! failure echo, and the optional vector layer.

pub mod echo;
pub mod embed;
pub mod hygiene;
pub mod record;
pub mod stats;
