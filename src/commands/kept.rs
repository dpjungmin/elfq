//! [`Kept`], which leaves what a command builds for the system to take back
//! when elfq ends, rather than freeing it a piece at a time.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the process ends once the command has answered, as the elfq
/// program does. Tests run many commands in one process, and leave it off.
static ENDS_AFTER_ANSWER: AtomicBool = AtomicBool::new(false);

/// Says that the process ends once the command has answered, so that what
/// the command has built needn't be freed.
pub fn end_after_answer() {
    ENDS_AFTER_ANSWER.store(true, Ordering::Relaxed);
}

/// A value that a command keeps until it has answered, such as the loaded
/// file or its graph. When the process ends after the answer, the value is
/// left for the system to take back with the rest of the process's memory,
/// at once, rather than freed a piece at a time, which takes a while in a
/// large file. A run of a command whose answer is thrown away, as
/// `arguments::refuse_range` runs `callers` to see whether it answers, goes
/// in [`freeing`], so that what it keeps is freed for the runs after it.
pub(super) struct Kept<T>(Option<T>);

impl<T> Kept<T> {
    pub(super) fn new(value: T) -> Kept<T> {
        Kept(Some(value))
    }

    /// The value, which is then freed as any other.
    pub(super) fn into_inner(mut self) -> T {
        self.0.take().expect("only dropping takes a kept value")
    }
}

impl<T> Deref for Kept<T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.0.as_ref().expect("only dropping takes a kept value")
    }
}

impl<T> DerefMut for Kept<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.0.as_mut().expect("only dropping takes a kept value")
    }
}

impl<T> Drop for Kept<T> {
    fn drop(&mut self) {
        if ENDS_AFTER_ANSWER.load(Ordering::Relaxed) {
            std::mem::forget(self.0.take());
        }
    }
}

/// Runs `trial`, whose answer is thrown away, freeing what it keeps in a
/// [`Kept`] once it is done with it.
pub(super) fn freeing<R>(trial: impl FnOnce() -> R) -> R {
    let ends = ENDS_AFTER_ANSWER.swap(false, Ordering::Relaxed);
    let result = trial();
    ENDS_AFTER_ANSWER.store(ends, Ordering::Relaxed);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_value_is_freed_unless_the_process_ends_after_the_answer() {
        use std::rc::Rc;

        let value = Rc::new(());
        let count = || Rc::strong_count(&value);
        // Tests leave end_after_answer off, so a kept value is freed.
        drop(Kept::new(Rc::clone(&value)));
        assert_eq!(count(), 1);
        // Turned on, only a value kept in a run that goes in freeing is.
        // Other tests that run meanwhile only free less.
        end_after_answer();
        freeing(|| drop(Kept::new(Rc::clone(&value))));
        let freed = count();
        drop(Kept::new(Rc::clone(&value)));
        let left = count();
        ENDS_AFTER_ANSWER.store(false, Ordering::Relaxed);
        assert_eq!((freed, left), (1, 2));
    }
}
