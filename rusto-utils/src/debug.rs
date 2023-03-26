use std::sync::Arc;

struct DebugArc<T> {
    ptr: *const DebugArcInner<T>,
}

impl<T> DebugArc<T> {
    fn from_ref(arc: &Arc<T>) -> &Self {
        let debug_arc_ptr: *const DebugArc<T> = arc as *const _ as _;
        // Safety: safe as long as Arc/ArcInner layout doesn't change
        unsafe { debug_arc_ptr.as_ref().unwrap() }
    }

    fn inner(&self) -> &DebugArcInner<T> {
        unsafe { self.ptr.as_ref().expect("Arc pointer should never be null") }
    }
}

impl<T> std::fmt::Display for DebugArc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Arc(strong={}, weak={})",
            self.inner()
                .strong
                .load(std::sync::atomic::Ordering::Acquire),
            self.inner().weak.load(std::sync::atomic::Ordering::Acquire)
        )
    }
}

#[repr(C)]
struct DebugArcInner<T> {
    strong: std::sync::atomic::AtomicUsize,
    weak: std::sync::atomic::AtomicUsize,
    data: T,
}

/// Inspect an owned `Arc<T>` and return a string with the current count
/// of strong and weak references.
///
/// # Examples
///
/// Inspecting an Arc:
///
/// ```
/// # use std::sync::Arc;
/// use rusto_utils::debug::debug_arc;
/// let r = Arc::new(100);
/// assert_eq!(debug_arc(&r), "Arc(strong=1, weak=1)");
/// ```
///
/// Inspecting an Arc with multiple references:
///
/// ```
/// # use std::sync::Arc;
/// use rusto_utils::debug::debug_arc;
/// let r = Arc::new(100);
/// let r2 = r.clone();
/// assert_eq!(debug_arc(&r), "Arc(strong=2, weak=1)");
/// drop(r2);
/// assert_eq!(debug_arc(&r), "Arc(strong=1, weak=1)");
/// ```
pub fn debug_arc<T>(arc: &Arc<T>) -> String {
    let debug_arc = DebugArc::from_ref(arc);
    debug_arc.to_string()
}
