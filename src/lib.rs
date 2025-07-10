use winapi::ctypes::c_void;
use std::mem::zeroed;
use std::ptr::null_mut;
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, AtomicU64, Ordering};
use winapi::shared::minwindef::{DWORD, LPVOID, FALSE};
use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
use winapi::um::heapapi::{GetProcessHeap, HeapAlloc, HeapFree};
use winapi::um::processthreadsapi::CreateThread;
use winapi::um::synchapi::{
    CreateSemaphoreW, ReleaseSemaphore, WaitForSingleObject,
    InitializeCriticalSection, EnterCriticalSection, LeaveCriticalSection,
    CreateEventW, SetEvent,
};
use winapi::um::sysinfoapi::{GetSystemInfo, SYSTEM_INFO};
use winapi::um::winbase::INFINITE;
use winapi::um::winnt::{HANDLE, HEAP_ZERO_MEMORY};
use winapi::um::sysinfoapi::GetTickCount64;

/// Function type for non-returning tasks
pub type TaskFn = unsafe extern "system" fn(param: *mut c_void);

// Maximum number of tasks in each queue
const MAX_QUEUE: usize = 1024;

/// Task priority level
pub enum Priority {
    High,
    Low,
}

/// Sentinel-function to terminate workers
const SENTINEL_FUNC: TaskFn = sentinel_function;
unsafe extern "system" fn sentinel_function(_param: *mut c_void) { /* no-op */ }

static INIT: Once = Once::new();

static mut QUEUE_HIGH: [QueueItem; MAX_QUEUE] = [QueueItem { func: None, param: null_mut(), enqueue_tick: 0 }; MAX_QUEUE];
static mut QUEUE_LOW: [QueueItem; MAX_QUEUE] = [QueueItem { func: None, param: null_mut(), enqueue_tick: 0 }; MAX_QUEUE];

static mut HEAD_HIGH: usize = 0;
static mut TAIL_HIGH: usize = 0;
static mut COUNT_HIGH: usize = 0;
static mut HEAD_LOW: usize = 0;
static mut TAIL_LOW: usize = 0;
static mut COUNT_LOW: usize = 0;

static mut COUNT_CS: winapi::um::minwinbase::CRITICAL_SECTION = winapi::um::minwinbase::CRITICAL_SECTION {
    DebugInfo: null_mut(),
    LockCount: 0,
    RecursionCount: 0,
    OwningThread: null_mut(),
    LockSemaphore: null_mut(),
    SpinCount: 0,
};

struct CsGuard {
    cs: *mut winapi::um::minwinbase::CRITICAL_SECTION,
}
impl CsGuard {
    unsafe fn new(cs: *mut winapi::um::minwinbase::CRITICAL_SECTION) -> Self {
        EnterCriticalSection(cs);
        CsGuard { cs }
    }
}
impl Drop for CsGuard {
    fn drop(&mut self) {
        unsafe { LeaveCriticalSection(self.cs); }
    }
}

static mut TASK_SEM: HANDLE = null_mut();
static mut SLOT_SEM: HANDLE = null_mut();

static mut THREAD_HANDLES: *mut HANDLE = null_mut();
static mut THREAD_COUNT: DWORD = 0;

#[repr(C)]
#[derive(Copy, Clone)]
struct QueueItem {
    func: Option<TaskFn>,
    param: *mut c_void,
    enqueue_tick: u64,
}

/// Main worker thread: processes high priority first, then low priority
unsafe extern "system" fn worker_thread(_param: LPVOID) -> DWORD {
    loop {
        WaitForSingleObject(TASK_SEM, INFINITE);
        let mut func_opt: Option<TaskFn> = None;
        let mut param: *mut c_void = null_mut();
        let mut enqueue_tick: u64 = 0;
        {
            let _guard = CsGuard::new(&raw mut COUNT_CS as *mut _);
            if COUNT_HIGH > 0 {
                let idx = HEAD_HIGH;
                func_opt = QUEUE_HIGH[idx].func;
                param = QUEUE_HIGH[idx].param;
                enqueue_tick = QUEUE_HIGH[idx].enqueue_tick;
                HEAD_HIGH = (HEAD_HIGH + 1) % MAX_QUEUE;
                COUNT_HIGH -= 1;
            } else if COUNT_LOW > 0 {
                let idx = HEAD_LOW;
                func_opt = QUEUE_LOW[idx].func;
                param = QUEUE_LOW[idx].param;
                enqueue_tick = QUEUE_LOW[idx].enqueue_tick;
                HEAD_LOW = (HEAD_LOW + 1) % MAX_QUEUE;
                COUNT_LOW -= 1;
            }
        }
        ReleaseSemaphore(SLOT_SEM, 1, null_mut());
        if let Some(func) = func_opt {
            if func as usize == SENTINEL_FUNC as usize {
                break;
            }
            let start_tick = GetTickCount64();
            if enqueue_tick > 0 && start_tick >= enqueue_tick {
                TOTAL_WAIT_MS.fetch_add(start_tick - enqueue_tick, Ordering::Relaxed);
            }
            let exec_start = GetTickCount64();
            let _ = std::panic::catch_unwind(|| {
                func(param);
            });
            let exec_end = GetTickCount64();
            TOTAL_EXEC_MS.fetch_add(exec_end - exec_start, Ordering::Relaxed);
            TOTAL_EXECUTED.fetch_add(1, Ordering::Relaxed);
        }
    }
    0
}

/// Initializes pool: CRITICAL_SECTION, semaphores, creates worker threads.
fn initialize_pool() {
    unsafe {
        InitializeCriticalSection(&raw mut COUNT_CS as *mut _);
        TASK_SEM = CreateSemaphoreW(null_mut(), 0, (MAX_QUEUE * 2) as i32, null_mut());
        SLOT_SEM = CreateSemaphoreW(null_mut(), (MAX_QUEUE * 2) as i32, (MAX_QUEUE * 2) as i32, null_mut());
        let mut sys: SYSTEM_INFO = zeroed();
        GetSystemInfo(&mut sys);
        THREAD_COUNT = sys.dwNumberOfProcessors;
        let size = (THREAD_COUNT as usize) * size_of::<HANDLE>();
        let heap = GetProcessHeap();
        THREAD_HANDLES = HeapAlloc(heap, HEAP_ZERO_MEMORY, size) as *mut HANDLE;
        for i in 0..THREAD_COUNT as usize {
            let h = CreateThread(null_mut(), 0, Some(worker_thread), null_mut(), 0, null_mut());
            *THREAD_HANDLES.add(i) = h;
        }
    }
}

/// Adds a task with priority. Blocks if queue is full.
pub fn submit_task_prioritized(func: TaskFn, param: *mut c_void, priority: Priority) {
    unsafe {
        INIT.call_once(|| initialize_pool());
        WaitForSingleObject(SLOT_SEM, INFINITE);
        let now_tick = GetTickCount64();
        TOTAL_ENQUEUED.fetch_add(1, Ordering::Relaxed);
        {
            let _guard = CsGuard::new(&raw mut COUNT_CS as *mut _);
            match priority {
                Priority::High => {
                    let idx = TAIL_HIGH;
                    QUEUE_HIGH[idx].func = Some(func);
                    QUEUE_HIGH[idx].param = param;
                    QUEUE_HIGH[idx].enqueue_tick = now_tick;
                    TAIL_HIGH = (TAIL_HIGH + 1) % MAX_QUEUE;
                    COUNT_HIGH += 1;
                }
                Priority::Low => {
                    let idx = TAIL_LOW;
                    QUEUE_LOW[idx].func = Some(func);
                    QUEUE_LOW[idx].param = param;
                    QUEUE_LOW[idx].enqueue_tick = now_tick;
                    TAIL_LOW = (TAIL_LOW + 1) % MAX_QUEUE;
                    COUNT_LOW += 1;
                }
            }
        }
        ReleaseSemaphore(TASK_SEM, 1, null_mut());
    }
}

/// Legacy API: adds Low priority
pub fn submit_task(func: TaskFn, param: *mut c_void) {
    submit_task_prioritized(func, param, Priority::Low);
}

/// Shuts down the pool
pub fn shutdown_pool() {
    unsafe {
        for _ in 0..THREAD_COUNT {
            WaitForSingleObject(SLOT_SEM, INFINITE);
            {
                let _guard = CsGuard::new(&raw mut COUNT_CS as *mut _);
                let idx = TAIL_HIGH;
                QUEUE_HIGH[idx].func = Some(SENTINEL_FUNC);
                QUEUE_HIGH[idx].param = null_mut();
                QUEUE_HIGH[idx].enqueue_tick = 0;
                TAIL_HIGH = (TAIL_HIGH + 1) % MAX_QUEUE;
                COUNT_HIGH += 1;
            }
            ReleaseSemaphore(TASK_SEM, 1, null_mut());
        }
        for i in 0..THREAD_COUNT as usize {
            let h = *THREAD_HANDLES.add(i);
            if !h.is_null() && h != INVALID_HANDLE_VALUE {
                WaitForSingleObject(h, INFINITE);
                CloseHandle(h);
            }
        }
        if !THREAD_HANDLES.is_null() {
            let heap = GetProcessHeap();
            HeapFree(heap, 0, THREAD_HANDLES as *mut c_void);
            THREAD_HANDLES = null_mut();
        }
        if !TASK_SEM.is_null() { CloseHandle(TASK_SEM); TASK_SEM = null_mut(); }
        if !SLOT_SEM.is_null() { CloseHandle(SLOT_SEM); SLOT_SEM = null_mut(); }
        // Do not delete CRITICAL_SECTION to allow reusing the pool for subsequent tasks
        // DeleteCriticalSection(&raw mut COUNT_CS as *mut _);
    }
}

// Monitoring and stats
static TOTAL_ENQUEUED: AtomicUsize = AtomicUsize::new(0);
static TOTAL_EXECUTED: AtomicUsize = AtomicUsize::new(0);
static TOTAL_WAIT_MS: AtomicU64 = AtomicU64::new(0);
static TOTAL_EXEC_MS: AtomicU64 = AtomicU64::new(0);

/// Reloads stats and queues (helper for tests or whatever)
pub fn reset_pool_stats() {
    unsafe {
        TOTAL_ENQUEUED.store(0, Ordering::Relaxed);
        TOTAL_EXECUTED.store(0, Ordering::Relaxed);
        TOTAL_WAIT_MS.store(0, Ordering::Relaxed);
        TOTAL_EXEC_MS.store(0, Ordering::Relaxed);
        let _guard = CsGuard::new(&raw mut COUNT_CS as *mut _);
        HEAD_HIGH = 0;
        TAIL_HIGH = 0;
        COUNT_HIGH = 0;
        HEAD_LOW = 0;
        TAIL_LOW = 0;
        COUNT_LOW = 0;
    }
}

/// Structure with pool stats
pub struct PoolStats {
    pub total_enqueued: usize,
    pub total_executed: usize,
    pub avg_wait_ms: f64,
    pub avg_exec_ms: f64,
    pub current_queue: usize,
    pub high_queue: usize,
    pub low_queue: usize,
}

/// Returns current stats of the pool
pub fn get_pool_stats() -> PoolStats {
    unsafe {
        let total_enq = TOTAL_ENQUEUED.load(Ordering::Relaxed);
        let total_exec = TOTAL_EXECUTED.load(Ordering::Relaxed);
        let total_wait = TOTAL_WAIT_MS.load(Ordering::Relaxed);
        let total_exec_time = TOTAL_EXEC_MS.load(Ordering::Relaxed);
        let _guard = CsGuard::new(&raw mut COUNT_CS as *mut _);
        let hq = COUNT_HIGH;
        let lq = COUNT_LOW;
        let avg_wait = if total_exec > 0 { total_wait as f64 / total_exec as f64 } else { 0.0 };
        let avg_exec_time = if total_exec > 0 { total_exec_time as f64 / total_exec as f64 } else { 0.0 };
        PoolStats {
            total_enqueued: total_enq,
            total_executed: total_exec,
            avg_wait_ms: avg_wait,
            avg_exec_ms: avg_exec_time,
            current_queue: hq + lq,
            high_queue: hq,
            low_queue: lq,
        }
    }
}

// API for running closures

#[repr(C)]
struct ClosureContext {
    callback: Option<Box<dyn FnOnce() + Send>>,
}

unsafe extern "system" fn closure_trampoline(param: *mut c_void) {
    let ctx = Box::from_raw(param as *mut ClosureContext);
    if let Some(f) = ctx.callback {
        f();
    }
    // Box calls its own drop (freeing automatically)
}

/// Adds closure to the pool
pub fn submit_closure<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        INIT.call_once(|| initialize_pool());
        let boxed = Box::new(ClosureContext { callback: Some(Box::new(f)) });
        let ptr = Box::into_raw(boxed) as *mut c_void;
        submit_task_prioritized(closure_trampoline, ptr, Priority::Low);
    }
}

#[cfg(test)]
mod closure_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_submit_closure_counter() {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        for _ in 0..5 {
            submit_closure(|| { COUNTER.fetch_add(1, Ordering::SeqCst); });
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(COUNTER.load(Ordering::SeqCst), 5);
    }
}

// API for functions that return a result

#[repr(C)]
struct ReturnContext {
    event: HANDLE,
    result: u64,
    callback: Option<Box<dyn FnOnce() -> u64 + Send>>,
}

unsafe extern "system" fn return_trampoline(param: *mut c_void) {
    let ctx = &mut *(param as *mut ReturnContext);
    if let Some(f) = ctx.callback.take() {
        ctx.result = f();
    }
    SetEvent(ctx.event);
}

/// Handle for getting the result
pub struct TaskHandle {
    event: HANDLE,
    ctx: *mut ReturnContext,
}

impl TaskHandle {
    /// Waits for an event and returns result
    pub fn get(self) -> u64 {
        unsafe {
            WaitForSingleObject(self.event, INFINITE);
            let res = (*self.ctx).result;
            CloseHandle(self.event);
            let _ = Box::from_raw(self.ctx);
            res
        }
    }
}

pub fn submit_task_with_return<F>(f: F) -> TaskHandle
where
    F: FnOnce() -> u64 + Send + 'static,
{
    unsafe {
        INIT.call_once(|| initialize_pool());
        let event = CreateEventW(null_mut(), FALSE, FALSE, null_mut());
        let boxed = Box::new(ReturnContext { event, result: 0, callback: Some(Box::new(f)) });
        let ptr = Box::into_raw(boxed) as *mut c_void;
        submit_task_prioritized(return_trampoline, ptr, Priority::Low);
        TaskHandle { event, ctx: ptr as *mut ReturnContext }
    }
}

#[cfg(test)]
mod return_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_return_value() {
        let handle = submit_task_with_return(|| 42u64);
        let res = handle.get();
        assert_eq!(res, 42);
    }

    #[test]
    fn test_multiple_returns() {
        let mut handles = Vec::new();
        for i in 0..4u64 {
            handles.push(submit_task_with_return(move || i * 10));
        }
        for (i, h) in handles.into_iter().enumerate() {
            assert_eq!(h.get(), (i as u64) * 10);
        }
    }

    #[test]
    fn test_priority_execution() {
        static ORDER: AtomicUsize = AtomicUsize::new(0);
        extern "system" fn high_task(param: *mut c_void) {
            let p = unsafe { &*(param as *const AtomicUsize) };
            p.store(1, Ordering::SeqCst);
        }
        extern "system" fn low_task(param: *mut c_void) {
            let p = unsafe { &*(param as *const AtomicUsize) };
            p.store(2, Ordering::SeqCst);
        }
        submit_task_prioritized(high_task as TaskFn,
                                &ORDER as *const _ as *mut c_void, Priority::High);
        submit_task_prioritized(low_task as TaskFn,
                                &ORDER as *const _ as *mut c_void, Priority::Low);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(ORDER.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_pool_stats() {
        reset_pool_stats();
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let initial = get_pool_stats();
        for _ in 0..10 {
            submit_closure(|| { COUNTER.fetch_add(1, Ordering::SeqCst); });
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        let stats = get_pool_stats();
        assert!(stats.total_enqueued - initial.total_enqueued >= 10);
        assert!(stats.total_executed - initial.total_executed >= 10);
        assert_eq!(COUNTER.load(Ordering::SeqCst), 10);
    }

    #[test]
    fn test_shutdown() {
        for _ in 0..5 {
            submit_closure(|| {});
        }
        shutdown_pool();
        assert!(true);
    }
}
