use std::sync::Arc;

/// Inspect an owned `Arc<T>` and return a string with the current count
/// of strong and weak references.
///
/// # Examples
///
/// Inspecting an Arc:
///
/// ```
/// # use std::sync::Arc;
/// use wotto_utils::debug::debug_arc;
/// let r = Arc::new(100);
/// assert_eq!(debug_arc(&r), "Arc(strong=1, weak=0)");
/// ```
///
/// Inspecting an Arc with multiple references:
///
/// ```
/// # use std::sync::Arc;
/// use wotto_utils::debug::debug_arc;
/// let r = Arc::new(100);
/// let r2 = r.clone();
/// let weak = Arc::downgrade(&r);
/// assert_eq!(debug_arc(&r), "Arc(strong=2, weak=1)");
/// drop(r2);
/// assert_eq!(debug_arc(&r), "Arc(strong=1, weak=1)");
/// drop(weak);
/// assert_eq!(debug_arc(&r), "Arc(strong=1, weak=0)");
/// ```
pub fn debug_arc<T>(arc: &Arc<T>) -> String {
    format!(
        "Arc(strong={}, weak={})",
        Arc::strong_count(arc),
        Arc::weak_count(arc)
    )
}
