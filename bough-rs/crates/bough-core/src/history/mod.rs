//! History (port of `src/history/`): the tag-history command memory and the
//! thread operations (fork/unsend/compact/…). Branch sources stay
//! byte-identical; the memory has one door and it is `bough tags`.

pub mod ops;
pub mod tags;
