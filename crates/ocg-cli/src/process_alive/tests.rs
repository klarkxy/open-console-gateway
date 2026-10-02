use super::process_is_running;

#[test]
fn current_process_is_running_and_zero_is_not() {
    assert!(process_is_running(std::process::id()));
    assert!(!process_is_running(0));
}
