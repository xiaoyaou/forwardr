#![cfg(loom)]

//! 需要配置环境变量`RUSTFLAGS="-cfg loom"`，`oneshot`中的标准库类型，需要替换为`loom`类型
//!
//! ```rust
//! #[cfg(loom)]
//! use loom::{
//!     cell::UnsafeCell,
//!     sync::atomic::{AtomicU32, Ordering::*},
//! };
//! ```

use loom::sync::Arc;
use loom::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use conlock::oneshot;

/// Test basic send and receive
#[test]
fn loom_basic_send_recv() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();

        let handle = loom::thread::spawn(move || {
            s.send(42).expect("send failed");
        });
        let value = r.recv();
        assert_eq!(value, Some(42));
        handle.join().unwrap();
    });
}

/// Test receiver drop before send
#[test]
fn loom_receiver_drop_before_send() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();

        let handle = loom::thread::spawn(move || {
            drop(r);
        });

        // Wait for receiver to be dropped
        handle.join().unwrap();

        let result = s.send(42);
        assert!(result.is_err());
    });
}

/// Test sender drop before receive
#[test]
fn loom_sender_drop_before_recv() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();

        let handle = loom::thread::spawn(move || {
            drop(s);
        });

        // Wait for sender to be dropped
        handle.join().unwrap();

        let value = r.recv();
        assert_eq!(value, None);
    });
}


/// Test rapid send and receive
#[test]
fn loom_rapid_send_recv() {
    loom::model(|| {
        for i in 0..2 {
            let (s, r) = oneshot::channel::<i32>();

            let handle = loom::thread::spawn(move || {
                s.send(i).unwrap();
            });

            let value = r.recv();
            assert_eq!(value, Some(i));
            handle.join().unwrap();
        }
    });
}

/// Test both sender and receiver drop
#[test]
fn loom_both_drop() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();

        let handle1 = loom::thread::spawn(move || {
            drop(s);
        });

        let handle2 = loom::thread::spawn(move || {
            drop(r);
        });

        handle1.join().unwrap();
        handle2.join().unwrap();
    });
}

/// Test send then recv then drop
#[test]
fn loom_send_recv_drop_sequence() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();

        let handle1 = loom::thread::spawn(move || {
            s.send(100).unwrap();
        });

        let handle2 = loom::thread::spawn(move || {
            let v = r.recv();
            assert_eq!(v, Some(100));
        });

        handle1.join().unwrap();
        handle2.join().unwrap();
    });
}

/// Test double recv on same receiver
#[test]
fn loom_double_recv() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();

        let handle = loom::thread::spawn(move || {
            s.send(42).unwrap();
        });

        let v1 = r.recv();
        let v2 = r.recv();

        assert_eq!(v1, Some(42));
        assert_eq!(v2, None);

        handle.join().unwrap();
    });
}

/// Test interleaved send and recv with thread parking/unparking
#[test]
fn loom_interleaved_operations() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();
        let received = Arc::new(AtomicUsize::new(0));
        let received_clone = Arc::clone(&received);

        let handle1 = loom::thread::spawn(move || {
            match r.recv() {
                Some(42) => received_clone.store(1, SeqCst),
                _ => unreachable!(),
            }
        });

        let handle2 = loom::thread::spawn(move || {
            loom::thread::yield_now();
            s.send(42).unwrap();
        });

        handle1.join().unwrap();
        handle2.join().unwrap();

        assert_eq!(received.load(SeqCst), 1);
    });
}

/// Test recv before send with waiting
#[test]
fn loom_recv_before_send() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<String>();

        let handle_recv = loom::thread::spawn(move || {
            // Start waiting before send
            r.recv()
        });

        let handle_send = loom::thread::spawn(move || {
            loom::thread::yield_now();
            s.send("hello".to_string()).unwrap();
        });

        let value = handle_recv.join().unwrap();
        assert_eq!(value, Some("hello".to_string()));
        handle_send.join().unwrap();
    });
}

/// Test with drop counter to verify cleanup
#[test]
fn loom_value_dropped_after_recv() {
    #[derive(Debug)]
    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, SeqCst);
        }
    }

    loom::model(|| {
        let (s, r) = oneshot::channel::<DropCounter>();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let handle = loom::thread::spawn(move || {
            s.send(DropCounter(counter_clone)).unwrap();
        });

        let value = r.recv();
        assert!(value.is_some());
        drop(value);

        handle.join().unwrap();

        // Counter should be incremented exactly once
        assert_eq!(counter.load(SeqCst), 1);
    });
}

/// Test sender drop leaves data unconsumed
#[test]
fn loom_sender_drop_unconsumed_data() {
    #[derive(Debug)]
    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, SeqCst);
        }
    }

    loom::model(|| {
        let (s, r) = oneshot::channel::<DropCounter>();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_send = Arc::clone(&counter);

        let handle_send = loom::thread::spawn(move || {
            s.send(DropCounter(counter_send)).unwrap();
        });

        let handle_recv = loom::thread::spawn(move || {
            loom::thread::yield_now();
            let v = r.recv();
            assert!(v.is_some());
        });

        handle_send.join().unwrap();
        handle_recv.join().unwrap();

        // Data should be dropped once
        assert_eq!(counter.load(SeqCst), 1);
    });
}

/// Test race: recv drop vs send
#[test]
fn loom_race_recv_drop_vs_send() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();

        let handle_recv = loom::thread::spawn(move || {
            drop(r);
        });

        handle_recv.join().unwrap();
        let handle_send = loom::thread::spawn(move || {
            s.send(42)
        });

        let result = handle_send.join().unwrap();

        // Send should fail because receiver is dropped
        assert!(result.is_err());
    });
}

/// Test race: send drop vs recv
#[test]
fn loom_race_send_drop_vs_recv() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();

        let handle_send = loom::thread::spawn(move || {
            drop(s);
        });

        let handle_recv = loom::thread::spawn(move || {
            r.recv()
        });

        handle_send.join().unwrap();
        let result = handle_recv.join().unwrap();

        // Recv should return None
        assert_eq!(result, None);
    });
}

/// Test repeated recv attempts
#[test]
fn loom_repeated_recv_attempts() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();

        let handle_send = loom::thread::spawn(move || {
            loom::thread::yield_now();
            s.send(42).unwrap();
        });

        let v1 = r.recv();
        assert_eq!(v1, Some(42));

        let v2 = r.recv();
        assert_eq!(v2, None);

        let v3 = r.recv();
        assert_eq!(v3, None);

        handle_send.join().unwrap();
    });
}

/// Test send error path
#[test]
fn loom_send_error_cleanup() {
    #[derive(Debug)]
    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, SeqCst);
        }
    }

    loom::model(|| {
        let (s, r) = oneshot::channel::<DropCounter>();
        let counter = Arc::new(AtomicUsize::new(0));

        let handle = loom::thread::spawn(move || {
            drop(r);
        });

        let counter_clone = Arc::clone(&counter);
        loom::thread::yield_now();
        loom::thread::yield_now();
        let result = s.send(DropCounter(counter_clone));

        assert!(result.is_err());
        handle.join().unwrap();
        drop(result);

        // Dropped value should be cleaned up immediately
        assert_eq!(counter.load(SeqCst), 1);
    });
}

/// Test concurrent drops cleanup properly
#[test]
fn loom_concurrent_cleanup() {
    #[derive(Debug)]
    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, SeqCst);
        }
    }

    loom::model(|| {
        let (s, r) = oneshot::channel::<DropCounter>();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_send = Arc::clone(&counter);

        let handle_send = loom::thread::spawn(move || {
            s.send(DropCounter(counter_send)).unwrap();
        });

        let handle_recv = loom::thread::spawn(move || {
            loom::thread::yield_now();
            let v = r.recv();
            drop(v);
        });

        handle_send.join().unwrap();
        handle_recv.join().unwrap();

        // Value should be dropped exactly once
        assert_eq!(counter.load(SeqCst), 1);
    });
}

/// Test memory ordering with explicit synchronization points
#[test]
fn loom_memory_ordering() {
    loom::model(|| {
        let counter = Arc::new(AtomicUsize::new(0));
        let (s, r) = oneshot::channel::<usize>();

        let counter_send = Arc::clone(&counter);
        let handle_send = loom::thread::spawn(move || {
            counter_send.store(42, SeqCst);
            s.send(100).unwrap();
        });

        let counter_recv = Arc::clone(&counter);
        let handle_recv = loom::thread::spawn(move || {
            let v = r.recv().unwrap();
            let before = counter_recv.load(SeqCst);
            (v, before)
        });

        handle_send.join().unwrap();
        let (val, mem) = handle_recv.join().unwrap();

        // Memory ordering should guarantee visibility
        assert_eq!(val, 100);
        assert_eq!(mem, 42);
    });
}

/// Test rapid allocation and deallocation
#[test]
fn loom_allocation_cleanup() {
    loom::model(|| {
        for i in 0..2 {
            let (s, r) = oneshot::channel::<i32>();

            let handle = loom::thread::spawn(move || {
                s.send(i).unwrap();
            });

            let v = r.recv();
            assert_eq!(v, Some(i));
            handle.join().unwrap();
        }
    });
}

/// Stress test with multiple operations
#[test]
fn loom_stress_basic_operations() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<u64>();

        let h1 = loom::thread::spawn(move || {
            s.send(0xDEADBEEFCAFEBABE).unwrap();
        });

        let h2 = loom::thread::spawn(move || {
            r.recv()
        });

        h1.join().unwrap();
        let val = h2.join().unwrap();
        assert_eq!(val, Some(0xDEADBEEFCAFEBABE));
    });
}

/// Test with zero-sized type
#[test]
fn loom_zst_channel() {
    #[derive(Debug, PartialEq)]
    struct ZST;

    loom::model(|| {
        let (s, r) = oneshot::channel::<ZST>();

        let handle = loom::thread::spawn(move || {
            s.send(ZST).unwrap();
        });

        let value = r.recv();
        assert_eq!(value, Some(ZST));
        handle.join().unwrap();
    });
}

/// Test with unit type
#[test]
fn loom_unit_channel() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<()>();

        let handle = loom::thread::spawn(move || {
            s.send(()).unwrap();
        });

        let value = r.recv();
        assert_eq!(value, Some(()));
        handle.join().unwrap();
    });
}

/// Test sender tries to send after receiver close
#[test]
fn loom_send_after_receiver_close() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();

        let handle = loom::thread::spawn(move || {
            drop(r);
        });

        loom::thread::yield_now();
        loom::thread::yield_now();
        let result = s.send(42);

        assert!(result.is_err());
        handle.join().unwrap();
    });
}

/// Test receiver tries to recv after sender drop
#[test]
fn loom_recv_after_sender_drop() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();

        let handle = loom::thread::spawn(move || {
            drop(s);
            loom::thread::yield_now();
        });

        loom::thread::yield_now();
        let value = r.recv();

        assert_eq!(value, None);
        handle.join().unwrap();
    });
}

/// Test complex state transitions
#[test]
fn loom_complex_state_transitions() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();

        let h1 = loom::thread::spawn(move || {
            loom::thread::yield_now();
            s.send(10).unwrap();
        });

        let h2 = loom::thread::spawn(move || {
            loom::thread::yield_now();
            r.recv()
        });

        let result = h2.join().unwrap();
        h1.join().unwrap();

        assert_eq!(result, Some(10));
    });
}

/// Test multiple recv calls return consistent results
#[test]
fn loom_multiple_recv_consistency() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();

        let handle_send = loom::thread::spawn(move || {
            s.send(123).unwrap();
        });

        // First recv gets the value
        let v1 = r.recv();
        assert_eq!(v1, Some(123));

        // All subsequent recvs should return None consistently
        for _ in 0..3 {
            let v = r.recv();
            assert_eq!(v, None, "subsequent recv must return None");
        }

        handle_send.join().unwrap();
    });
}

/// Test drop cleanup with DropCounter - verify no double drop
#[test]
fn loom_no_double_drop() {
    #[derive(Debug)]
    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, SeqCst);
        }
    }

    loom::model(|| {
        let (s, r) = oneshot::channel::<DropCounter>();
        let counter = Arc::new(AtomicUsize::new(0));

        let counter_clone = Arc::clone(&counter);
        let h_send = loom::thread::spawn(move || {
            s.send(DropCounter(counter_clone)).unwrap();
        });

        let h_recv = loom::thread::spawn(move || {
            r.recv()
        });

        h_send.join().unwrap();
        let received = h_recv.join().unwrap();

        // Ensure value was received
        assert!(received.is_some());
        drop(received);

        // Should be dropped exactly once
        assert_eq!(
            counter.load(SeqCst),
            1,
            "value must be dropped exactly once"
        );
    });
}

/// Test drop behavior when data not consumed
#[test]
fn loom_data_dropped_when_not_consumed() {
    #[derive(Debug)]
    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, SeqCst);
        }
    }

    loom::model(|| {
        let (s, r) = oneshot::channel::<DropCounter>();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_send = Arc::clone(&counter);

        let h1 = loom::thread::spawn(move || {
            s.send(DropCounter(counter_send)).unwrap();
        });

        let h2 = loom::thread::spawn(move || {
            loom::thread::yield_now();
            // Never call recv(), just drop receiver
            drop(r);
        });

        h1.join().unwrap();
        h2.join().unwrap();

        // Data should still be dropped exactly once (by receiver's drop_receiver)
        assert_eq!(counter.load(SeqCst), 1);
    });
}

/// Test race: recv tries to read while send is writing
#[test]
fn loom_recv_send_data_race() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<u64>();
        let received = Arc::new(AtomicUsize::new(0));

        let received_clone = Arc::clone(&received);
        let h_recv = loom::thread::spawn(move || {
            if let Some(v) = r.recv() {
                received_clone.store(v as usize, SeqCst);
            }
        });

        let h_send = loom::thread::spawn(move || {
            loom::thread::yield_now();
            s.send(0xABCDEF).unwrap();
        });

        h_recv.join().unwrap();
        h_send.join().unwrap();

        // Should have received exact value due to Release/Acquire ordering
        assert_eq!(received.load(SeqCst), 0xABCDEF);
    });
}

/// Test receiver drop while sender is sending
#[test]
fn loom_receiver_drop_concurrent_send() {
    #[derive(Debug)]
    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, SeqCst);
        }
    }

    loom::model(|| {
        let (s, r) = oneshot::channel::<DropCounter>();
        let counter = Arc::new(AtomicUsize::new(0));

        let counter_send = Arc::clone(&counter);

        let h_recv = loom::thread::spawn(move || {
            drop(r);
        });
        let h_send = loom::thread::spawn(move || {
            loom::thread::yield_now();
            s.send(DropCounter(counter_send))
        });

        let send_result = h_send.join().unwrap();
        h_recv.join().unwrap();

        // Send should fail (receiver dropped)
        assert!(send_result.is_err());
        drop(send_result);

        // Value in Err(val) should be cleaned up
        assert_eq!(counter.load(SeqCst), 1);
    });
}

/// Test state consistency: no leaks with multiple rapid iterations
#[test]
fn loom_no_resource_leak() {
    #[derive(Debug)]
    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, SeqCst);
        }
    }

    loom::model(|| {
        let iterations = 2;
        let total_counter = Arc::new(AtomicUsize::new(0));

        for i in 0..iterations {
            let (s, r) = oneshot::channel::<DropCounter>();
            let counter = Arc::new(AtomicUsize::new(0));

            let h1 = loom::thread::spawn({
                let counter_clone = Arc::clone(&counter);
                move || {
                    s.send(DropCounter(counter_clone)).unwrap();
                }
            });

            let h2 = loom::thread::spawn(move || {
                let _v = r.recv();
            });

            h1.join().unwrap();
            h2.join().unwrap();

            assert_eq!(counter.load(SeqCst), 1, "iteration {}: no resource leaks", i);
            total_counter.fetch_add(counter.load(SeqCst), SeqCst);
        }

        assert_eq!(
            total_counter.load(SeqCst),
            iterations,
            "total drops must match iterations"
        );
    });
}

/// Test send-recv with explicit yield points
#[test]
fn loom_explicit_yield_scheduling() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();
        let ready = Arc::new(AtomicUsize::new(0));

        let ready_clone = Arc::clone(&ready);
        let h1 = loom::thread::spawn(move || {
            loom::thread::yield_now();
            loom::thread::yield_now();
            // Give plenty of opportunity for other thread to act
            match r.recv() {
                Some(v) => {
                    ready_clone.store(1, SeqCst);
                    v
                }
                None => 0,
            }
        });

        let h2 = loom::thread::spawn(move || {
            loom::thread::yield_now();
            s.send(77).unwrap();
        });

        let result = h1.join().unwrap();
        h2.join().unwrap();

        assert_eq!(result, 77);
        assert_eq!(ready.load(SeqCst), 1);
    });
}

/// Test channel works correctly after full lifecycle
#[test]
fn loom_complete_lifecycle() {
    loom::model(|| {
        // Phase 1: Basic send/recv
        let (s, r) = oneshot::channel::<i32>();

        let h1 = loom::thread::spawn(move || {
            s.send(42).unwrap();
        });

        let h2 = loom::thread::spawn(move || {
            let v = r.recv();
            assert_eq!(v, Some(42));
            // Try to recv again
            assert_eq!(r.recv(), None);
        });

        h1.join().unwrap();
        h2.join().unwrap();

        // Phase 2: New channel, test error path
        let (s2, r2) = oneshot::channel::<i32>();
        drop(r2);
        let result = s2.send(99);
        assert!(result.is_err());
    });
}

/// Test interleaved recv/send with state verification
#[test]
fn loom_interleaved_state_transitions() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();
        let state_marker = Arc::new(AtomicUsize::new(0));

        let state_clone = Arc::clone(&state_marker);
        let h_recv = loom::thread::spawn(move || {
            state_clone.store(1, SeqCst);  // Mark recv start
            match r.recv() {
                Some(42) => state_clone.store(2, SeqCst),  // Mark recv success
                _ => state_clone.store(3, SeqCst),  // Mark recv failure
            }
        });

        let h_send = loom::thread::spawn(move || {
            loom::thread::yield_now();
            s.send(42).unwrap();
        });

        h_recv.join().unwrap();
        h_send.join().unwrap();

        assert_eq!(state_marker.load(SeqCst), 2);
    });
}

/// Test recv that starts before send completes
#[test]
fn loom_recv_during_send_setup() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();
        let received_flag = Arc::new(AtomicUsize::new(0));

        let flag_clone = Arc::clone(&received_flag);
        let h_recv = loom::thread::spawn(move || {
            // Try to recv immediately
            if let Some(v) = r.recv() {
                flag_clone.store(v as usize, SeqCst);
            }
        });

        let h_send = loom::thread::spawn(move || {
            // Delay slightly
            loom::thread::yield_now();
            loom::thread::yield_now();
            s.send(999).unwrap();
        });

        h_recv.join().unwrap();
        h_send.join().unwrap();

        assert_eq!(received_flag.load(SeqCst), 999);
    });
}

/// Test multiple send attempts (only first should succeed)
#[test]
fn loom_send_once_guarantee() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();
        let attempt_count = Arc::new(AtomicUsize::new(0));

        let h_recv = loom::thread::spawn(move || {
            let v = r.recv();
            assert_eq!(v, Some(1));  // Should get first sent value
        });

        let count_clone = Arc::clone(&attempt_count);
        let h_send = loom::thread::spawn(move || {
            let res1 = s.send(1);
            count_clone.fetch_add(if res1.is_ok() { 1 } else { 0 }, SeqCst);
        });

        h_recv.join().unwrap();
        h_send.join().unwrap();

        // Exactly one successful send
        assert_eq!(attempt_count.load(SeqCst), 1);
    });
}

/// Test receiver waiting while sender is still preparing
#[test]
fn loom_receiver_waits_for_sender() {
    loom::model(|| {
        let (s, r) = oneshot::channel::<i32>();
        let receiver_ready = Arc::new(AtomicUsize::new(0));

        let ready_clone = Arc::clone(&receiver_ready);
        let h_recv = loom::thread::spawn(move || {
            ready_clone.store(1, SeqCst);  // Signal receiver is ready
            r.recv()  // Block and wait
        });

        loom::thread::yield_now();  // Give receiver time to reach the wait point

        let h_send = loom::thread::spawn(move || {
            s.send(555).unwrap()
        });

        let result = h_recv.join().unwrap();
        h_send.join().unwrap();

        assert_eq!(receiver_ready.load(SeqCst), 1);
        assert_eq!(result, Some(555));
    });
}

/// Test rapid drop and recreate pattern
#[test]
fn loom_drop_recreate_pattern() {
    loom::model(|| {
        for iteration in 0..2 {
            let (s, r) = oneshot::channel::<i32>();

            let h1 = loom::thread::spawn(move || {
                s.send(iteration * 10 + 5).unwrap()
            });

            let h2 = loom::thread::spawn(move || {
                r.recv()
            });

            let result = h2.join().unwrap();
            h1.join().unwrap();

            assert_eq!(result, Some(iteration as i32 * 10 + 5));
        }
    });
}

/// Test cleanup with data that has side effects on drop
#[test]
fn loom_drop_side_effects() {
    #[derive(Debug)]
    struct SideEffectCounter {
        counter: Arc<AtomicUsize>,
    }

    impl Drop for SideEffectCounter {
        fn drop(&mut self) {
            self.counter.fetch_add(1, SeqCst);
        }
    }

    loom::model(|| {
        let (s, r) = oneshot::channel::<SideEffectCounter>();
        let counter = Arc::new(AtomicUsize::new(0));

        let counter_send = Arc::clone(&counter);
        let h_send = loom::thread::spawn(move || {
            s.send(SideEffectCounter {
                counter: counter_send,
            })
                .unwrap()
        });

        let h_recv = loom::thread::spawn(move || {
            let v = r.recv();
            // Let the value go out of scope
            drop(v);
        });

        h_send.join().unwrap();
        h_recv.join().unwrap();

        // Should be exactly 1 drop
        assert_eq!(counter.load(SeqCst), 1);
    });
}

/// Test proper cleanup with ownership transfer
#[test]
fn loom_ownership_transfer_cleanup() {
    #[derive(Debug)]
    struct OwnedResource {
        id: usize,
        cleanup_counter: Arc<AtomicUsize>,
    }

    impl Drop for OwnedResource {
        fn drop(&mut self) {
            self.cleanup_counter.fetch_add(1, SeqCst);
        }
    }

    loom::model(|| {
        let (s, r) = oneshot::channel::<OwnedResource>();
        let counter = Arc::new(AtomicUsize::new(0));

        let counter_clone = Arc::clone(&counter);
        let h_send = loom::thread::spawn(move || {
            s.send(OwnedResource {
                id: 42,
                cleanup_counter: counter_clone,
            })
                .unwrap()
        });

        let h_recv = loom::thread::spawn(move || {
            if let Some(res) = r.recv() {
                assert_eq!(res.id, 42);
                // Resource ownership transferred, will be cleaned when leaving scope
            }
        });

        h_send.join().unwrap();
        h_recv.join().unwrap();

        // Exactly one cleanup
        assert_eq!(counter.load(SeqCst), 1);
    });
}
