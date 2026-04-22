//! Windows：MFT 枚举（`FSCTL_ENUM_USN_DATA`）与 USN 增量。

mod mft;
#[cfg(windows)]
mod volumes;
#[cfg(windows)]
mod open_by_id;
mod metadata_fill;
mod nt_dir_query;
#[cfg(windows)]
mod usn;
#[cfg(windows)]
mod full_index_build;

pub use mft::{scan_volume, scan_volume_fast, MftScanner, SCAN_LIVE_ENTRIES};
pub use metadata_fill::{fill_metadata_by_id_pooled, MetaUpdate};
pub use nt_dir_query::{fetch_dir_meta_batched, DirMetaRec};
#[cfg(windows)]
pub use full_index_build::{build_full_disk_index, resolve_volume_list};
#[cfg(windows)]
pub use volumes::enumerate_local_drive_letters;
#[cfg(windows)]
pub use usn::{
    get_volume_serial_number, usn_watch_forever, UsnJournalWatcher, UsnResume, UsnState,
    UsnWatchMsg,
};
