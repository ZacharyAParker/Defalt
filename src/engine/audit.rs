//! A counting allocator for the test build, so a test can prove a code path
//! the audio thread runs never touches the heap.
//!
//! Counting is per thread and off unless a test turns it on, so tests
//! running in parallel do not see each other's allocations.

struct AllocationAudit;

thread_local! {
    static AUDIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ALLOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FREES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn counting() -> bool {
    AUDIT.try_with(|flag| flag.get()).unwrap_or(false)
}

unsafe impl std::alloc::GlobalAlloc for AllocationAudit {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if counting() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
        std::alloc::GlobalAlloc::alloc(&std::alloc::System, layout)
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: std::alloc::Layout) {
        if counting() {
            let _ = FREES.try_with(|count| count.set(count.get() + 1));
        }
        std::alloc::GlobalAlloc::dealloc(&std::alloc::System, p, layout)
    }
    unsafe fn realloc(&self, p: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        if counting() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
        std::alloc::GlobalAlloc::realloc(&std::alloc::System, p, layout, size)
    }
}

#[global_allocator]
static ALLOCATOR: AllocationAudit = AllocationAudit;

/// A running audit. `stop` ends it and returns the allocations it saw;
/// `stop_with_frees` returns frees as well, which the audio thread must not
/// do either.
pub struct Audit;

pub fn start() -> Audit {
    ALLOCATIONS.with(|count| count.set(0));
    FREES.with(|count| count.set(0));
    AUDIT.with(|flag| flag.set(true));
    Audit
}

impl Audit {
    pub fn stop(self) -> usize {
        self.stop_with_frees().0
    }

    pub fn stop_with_frees(self) -> (usize, usize) {
        AUDIT.with(|flag| flag.set(false));
        (ALLOCATIONS.with(|count| count.get()), FREES.with(|count| count.get()))
    }
}
