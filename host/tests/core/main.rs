//! BEHAVIOR of the shared core that wraps every module call: variable
//! resolution and history recording. Core stays in the host; modules plug
//! into it.

mod history;
mod variables;
