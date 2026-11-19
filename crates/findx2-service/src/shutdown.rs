//! Checkpoint result validation, NOT a final RAM durability receipt.
//! A receipt additionally needs admission fencing/draining of EVERY producer,
//! a consistent mutation sequence, published generation and instance/nonce binding.
//! No upgrade capability is enabled by this module.
use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CheckpointFailure {
    Writer(String),
    Busy,
    NewerMutations,
}
impl fmt::Display for CheckpointFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Writer(e) => write!(f, "shutdown checkpoint IO failed: {e}"),
            Self::Busy => write!(f, "shutdown checkpoint deferred: writer busy; no final receipt"),
            Self::NewerMutations => write!(f, "newer RAM mutations remain after checkpoint; no final receipt"),
        }
    }
}
impl std::error::Error for CheckpointFailure {}

pub(crate) fn require_checkpoint_result<E: fmt::Display>(
    result: Result<bool, E>, pending: impl FnOnce() -> bool,
) -> Result<(), CheckpointFailure> {
    match result {
        Err(e) => Err(CheckpointFailure::Writer(e.to_string())),
        Ok(false) => Err(CheckpointFailure::Busy),
        Ok(true) if pending() => Err(CheckpointFailure::NewerMutations),
        Ok(true) => Ok(()), // IO observation only. NEVER serialize as a final-save ACK.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shutdown_checkpoint_preserves_io_failure_without_consulting_clean_hint() {
        for failure in ["disk full", "short write", "access denied", "flush failed", "rename failed", "save interrupted"] {
            let result = require_checkpoint_result(Err::<bool, _>(failure), || panic!("an error cannot become an ACK"));
            assert_eq!(result, Err(CheckpointFailure::Writer(failure.into())));
        }
    }
    #[test]
    fn shutdown_checkpoint_busy_is_not_success() {
        assert_eq!(require_checkpoint_result(Ok::<_, &str>(false), || false), Err(CheckpointFailure::Busy));
    }
    #[test]
    fn shutdown_checkpoint_concurrent_mutation_is_not_clean() {
        let pending = std::sync::atomic::AtomicBool::new(false);
        let write_result = { pending.store(true, std::sync::atomic::Ordering::SeqCst); Ok::<_, &str>(true) };
        assert_eq!(require_checkpoint_result(write_result, || pending.load(std::sync::atomic::Ordering::SeqCst)), Err(CheckpointFailure::NewerMutations));
    }
    #[test]
    fn shutdown_checkpoint_clean_io_is_only_an_observation() {
        assert_eq!(require_checkpoint_result(Ok::<_, &str>(true), || false), Ok(()));
    }
}
