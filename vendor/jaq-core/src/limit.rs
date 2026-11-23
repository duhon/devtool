//! Evaluation limits (vendored patch, not upstream jaq).
//!
//! Every evaluation step spends one unit of fuel and nests one level deeper
//! while it runs. A step is running a term (`run`, `paths`, `update`) or
//! pulling a value from the output stream of a call (or of `..`).
//! When the fuel is spent or the nesting exceeds the depth limit,
//! the evaluation yields an error, and every later step fails too,
//! so that the evaluation terminates even if the error is caught.
//!
//! Limits are per thread; without [`set`], evaluation is unlimited.

use crate::box_iter::BoxIter;
use crate::exn::Exn;
use crate::Error;
use alloc::boxed::Box;
use alloc::string::String;

/// The limit that stopped an evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tripped {
    /// More than the given number of steps.
    Steps(u64),
    /// Nesting deeper than the given depth.
    Depth(usize),
}

#[cfg(feature = "std")]
mod state {
    use super::Tripped;
    use core::cell::Cell;

    pub struct State {
        pub fuel: Cell<u64>,
        pub steps: Cell<u64>,
        pub depth: Cell<usize>,
        pub max_depth: Cell<usize>,
        pub tripped: Cell<Option<Tripped>>,
    }

    std::thread_local! {
        pub static STATE: State = const {
            State {
                fuel: Cell::new(u64::MAX),
                steps: Cell::new(u64::MAX),
                depth: Cell::new(0),
                max_depth: Cell::new(usize::MAX),
                tripped: Cell::new(None),
            }
        };
    }
}

/// Limit evaluations on this thread to `steps` steps and `depth` nesting levels,
/// and clear the state of any previous evaluation.
pub fn set(steps: u64, depth: usize) {
    #[cfg(feature = "std")]
    state::STATE.with(|s| {
        s.fuel.set(steps);
        s.steps.set(steps);
        s.depth.set(0);
        s.max_depth.set(depth);
        s.tripped.set(None);
    });
    #[cfg(not(feature = "std"))]
    let _ = (steps, depth);
}

/// Remove the limits of this thread.
pub fn clear() {
    set(u64::MAX, usize::MAX);
}

/// The limit that stopped an evaluation on this thread since the last [`set`], if any.
pub fn tripped() -> Option<Tripped> {
    #[cfg(feature = "std")]
    return state::STATE.with(|s| s.tripped.get());
    #[cfg(not(feature = "std"))]
    None
}

/// Nesting level of one evaluation step; leaving it ends the step.
pub(crate) struct Step(());

impl Drop for Step {
    fn drop(&mut self) {
        #[cfg(feature = "std")]
        state::STATE.with(|s| s.depth.set(s.depth.get() - 1));
    }
}

fn error<'a, V: From<String>>(tripped: Tripped) -> Exn<'a, V> {
    use alloc::format;
    Exn::from(Error::str(match tripped {
        Tripped::Steps(n) => format!("evaluation stopped: more than {n} steps"),
        Tripped::Depth(n) => format!("evaluation stopped: recursion deeper than {n}"),
    }))
}

/// Spend one step, or fail when a limit is reached.
#[inline]
pub(crate) fn step<'a, V: From<String>>() -> Result<Step, Exn<'a, V>> {
    #[cfg(feature = "std")]
    return state::STATE.with(|s| {
        let (fuel, depth) = (s.fuel.get(), s.depth.get());
        if fuel == 0 || depth >= s.max_depth.get() {
            return Err(trip(s));
        }
        s.fuel.set(fuel - 1);
        s.depth.set(depth + 1);
        Ok(Step(()))
    });
    #[cfg(not(feature = "std"))]
    Ok(Step(()))
}

/// Record the limit reached, and stop all further steps.
#[cfg(feature = "std")]
#[cold]
fn trip<'a, V: From<String>>(s: &state::State) -> Exn<'a, V> {
    let tripped = s.tripped.get().unwrap_or(if s.fuel.get() == 0 {
        Tripped::Steps(s.steps.get())
    } else {
        Tripped::Depth(s.max_depth.get())
    });
    s.tripped.set(Some(tripped));
    s.fuel.set(0);
    error(tripped)
}

/// Limit the output stream of a step, unless it yields at most one value.
///
/// Such a stream is pulled at most once (and mostly evaluated already).
#[inline]
pub(crate) fn limit<'a, T: 'a, V: From<String> + 'a>(
    ys: BoxIter<'a, Result<T, Exn<'a, V>>>,
) -> BoxIter<'a, Result<T, Exn<'a, V>>> {
    if ys.size_hint().1.is_some_and(|n| n <= 1) {
        ys
    } else {
        Box::new(Limited::new(ys))
    }
}

/// Run `f` as one step with a limited output stream.
#[inline]
pub(crate) fn limited<'a, T: 'a, V: From<String> + 'a>(
    f: impl FnOnce() -> BoxIter<'a, Result<T, Exn<'a, V>>>,
) -> BoxIter<'a, Result<T, Exn<'a, V>>> {
    match step() {
        Ok(_step) => limit(f()),
        Err(e) => Box::new(core::iter::once(Err(e))),
    }
}

/// An output stream whose every pull is an evaluation step.
pub(crate) struct Limited<I>(Option<I>);

impl<I> Limited<I> {
    pub(crate) fn new(iter: I) -> Self {
        Self(Some(iter))
    }
}

impl<'a, T, V: From<String>, I: Iterator<Item = Result<T, Exn<'a, V>>>> Iterator for Limited<I> {
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        let iter = self.0.as_mut()?;
        match step() {
            Ok(_step) => iter.next(),
            Err(e) => {
                self.0 = None;
                Some(Err(e))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0
            .as_ref()
            .map_or((0, Some(0)), |iter| iter.size_hint())
    }
}
