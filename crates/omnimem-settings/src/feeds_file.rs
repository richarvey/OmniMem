//! The reading list on disk: `feeds.yml`. The engine owns reading and
//! writing it, since backups carry and restore it too.

pub(crate) use omnimem_engine::feeds_file::{load, save};
