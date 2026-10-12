//! Pure domain rules. No HTTP, no SQL, no clock access: every function takes
//! its inputs (including `now`) explicitly so it can be unit-tested.

pub mod calendar;
pub mod capacity;
pub mod channel;
pub mod election;
pub mod vote;
