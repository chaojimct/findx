//! Production publication predicates shared by retained tables and painted events.
pub(super) fn scope_current(published: Option<(u64,u64)>, requested: (u64,u64), pending: bool) -> bool {
    !pending && published == Some(requested)
}
pub(super) fn painted_event_current(painted: Option<(u64,u64)>, current: Option<(u64,u64)>) -> bool {
    painted.is_some() && painted == current
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn idle_busy_ready_error_cancel_and_rapid_replacement_matrix() {
        let old=(1,0); let new=(2,0); let rapid=(3,0);
        assert!(scope_current(Some(old),old,false));
        assert!(!scope_current(Some(old),new,true));
        assert!(!scope_current(Some(old),new,false)); // even between response and publish
        assert!(scope_current(Some(new),new,false));
        assert!(!scope_current(None,new,false)); // terminal error/cancel has ended pending
        assert!(!scope_current(Some(new),rapid,true));
        assert!(!scope_current(Some(new),rapid,false)); // late old success never revives it
        assert!(scope_current(Some(rapid),rapid,false));
    }
    #[test] fn ime_filter_epoch_invalidates_actions_without_dropping_old_rows() {
        assert!(!scope_current(Some((4,8)),(4,9),false));
        assert!(!scope_current(Some((4,9)),(5,9),false));
        assert!(scope_current(Some((5,10)),(5,10),false));
    }
    #[test] fn painted_row_callbacks_cannot_act_on_new_same_index_or_pending_scope() {
        for current in [None,Some((8,0)),Some((7,2))] {
            assert!(!painted_event_current(Some((7,1)),current));
        }
        assert!(!painted_event_current(None,None));
        assert!(!painted_event_current(None,Some((8,0))));
        assert!(painted_event_current(Some((8,0)),Some((8,0))));
    }
}
