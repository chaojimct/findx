//! Pinned DiskTree core/viewport port. No MFT, deletion, shell actions, SSH, or service access.
#![allow(dead_code)]
pub mod tree;
pub mod treemap;
pub mod filter;
pub mod classify;
pub mod viewport;
pub(crate) mod scan_preview;
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_,T> {m.lock().unwrap_or_else(|e|e.into_inner())}
