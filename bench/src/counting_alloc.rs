//! A global allocator that counts. Used only by the `mem` binary, so the
//! timing binary runs on the plain system allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

pub struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

fn add(n: usize) {
    let now = CURRENT.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(now, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
            add(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        CURRENT.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        let q = System.realloc(p, l, new_size);
        if !q.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
            CURRENT.fetch_sub(l.size(), Relaxed);
            add(new_size);
        }
        q
    }
}

/// What one closure did to the heap.
#[derive(Debug, Clone, Copy)]
pub struct Usage {
    /// Allocator calls (alloc and realloc).
    pub allocs: usize,
    /// Highest live bytes above the starting level while the closure ran.
    pub peak_bytes: usize,
    /// Live bytes still held when the closure returned, above the starting level.
    pub retained_bytes: isize,
}

/// Measure `f`. The value it returns stays alive until after the reading, so
/// `retained_bytes` is the footprint of that value.
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, Usage) {
    let base = CURRENT.load(Relaxed);
    PEAK.store(base, Relaxed);
    let allocs0 = ALLOCS.load(Relaxed);
    let r = f();
    let usage = Usage {
        allocs: ALLOCS.load(Relaxed) - allocs0,
        peak_bytes: PEAK.load(Relaxed).saturating_sub(base),
        retained_bytes: CURRENT.load(Relaxed) as isize - base as isize,
    };
    (r, usage)
}
