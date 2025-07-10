use win_rust_pool::{
    submit_task_prioritized, submit_closure,
    submit_task_with_return, shutdown_pool,
    get_pool_stats, Priority,
};
use winapi::ctypes::c_void;

unsafe extern "system" fn raw_task(param: *mut c_void) {
    let n = param as usize;
    println!("[Raw] Task executed with param: {}", n);
}

fn main() {
    println!("Submitting raw high-priority tasks...");
    for i in 0..5 {
        submit_task_prioritized(raw_task, i as *mut c_void, Priority::High);
    }

    println!("Submitting closure tasks...");
    for i in 0..5 {
        submit_closure(move || {
            println!("[Closure] Task {} executed", i);
        });
    }

    println!("Submitting tasks with return values...");
    let mut handles = Vec::new();
    for i in 1..=5 {
        let handle = submit_task_with_return(move || {
            println!("[Return] Computing square of {}", i);
            (i * i) as u64
        });
        handles.push((i, handle));
    }

    for (i, h) in handles {
        let result = h.get();
        println!("[Return] Result for {}: {}", i, result);
    }

    std::thread::sleep(std::time::Duration::from_millis(200));

    let stats = get_pool_stats();
    println!("\nThreadPool Statistics:");
    println!("  Total enqueued: {}", stats.total_enqueued);
    println!("  Total executed: {}", stats.total_executed);
    println!("  Average wait (ms): {:.2}", stats.avg_wait_ms);
    println!("  Average exec (ms): {:.2}", stats.avg_exec_ms);
    println!("  Current queue length: {} (high: {}, low: {})", stats.current_queue, stats.high_queue, stats.low_queue);

    println!("Shutting down thread pool...");
    shutdown_pool();
    println!("Done.");
}
