use autra_core::{BudgetSnapshot, CancelFlag};

#[test]
fn parent_cancellation_reaches_a_child_flag() {
    let parent = CancelFlag::new();
    let child = CancelFlag::child_of(&parent);
    assert!(!child.is_cancelled());

    parent.cancel();

    assert!(parent.is_cancelled());
    assert!(child.is_cancelled());
}

#[test]
fn child_cancellation_does_not_reach_the_parent() {
    let parent = CancelFlag::new();
    let child = CancelFlag::child_of(&parent);

    child.cancel();

    assert!(child.is_cancelled());
    assert!(!parent.is_cancelled());
}

#[test]
fn reserve_child_charges_the_parent_up_front() {
    let mut parent = BudgetSnapshot::new(4, 4);

    let child = parent.reserve_child(2, 2).unwrap();

    assert_eq!(child.max_turns, 2);
    assert_eq!(child.max_tool_calls, 2);
    assert_eq!(parent.turns_used, 1);
    assert_eq!(parent.tool_calls_used, 2);
    assert!(parent.reserve_child(2, 3).is_none());
}

#[test]
fn reserve_child_fails_when_the_parent_budget_is_exhausted() {
    let mut parent = BudgetSnapshot::new(1, 2);
    parent.turns_used = 1;

    assert!(parent.reserve_child(1, 1).is_none());
}
