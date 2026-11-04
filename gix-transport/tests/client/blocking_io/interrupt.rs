use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use gix_transport::client::InterruptHandle;

fn counting_handle() -> (InterruptHandle, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = InterruptHandle::new({
        let calls = calls.clone();
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
        }
    });
    (handle, calls)
}

#[test]
fn a_flag_that_was_set_before_watching_interrupts_before_the_operation_starts() {
    let (handle, calls) = counting_handle();
    let calls_seen_by_operation = handle.watch(&AtomicBool::new(true), || calls.load(Ordering::SeqCst));
    assert_eq!(
        calls_seen_by_operation, 1,
        "a quick operation can't finish before the watcher looks at the flag, so the transport was interrupted already"
    );
}

#[test]
fn a_flag_that_is_never_set_doesnt_interrupt() {
    let (handle, calls) = counting_handle();
    let value = handle.watch(&AtomicBool::new(false), || {
        std::thread::sleep(Duration::from_millis(60));
        7
    });
    assert_eq!(value, 7, "the result of the operation is returned");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "nothing was interrupted");
}

#[test]
fn a_flag_that_is_set_during_the_operation_interrupts_it_promptly() {
    let (handle, calls) = counting_handle();
    let flag = AtomicBool::new(false);
    let start = Instant::now();
    handle.watch(&flag, || {
        flag.store(true, Ordering::SeqCst);
        while calls.load(Ordering::SeqCst) == 0 {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the watcher notices the flag within its interval"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1, "interrupted exactly once");
}
