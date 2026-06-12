// Match the lib crate: CI denies clippy pedantic/nursery via `-D warnings`,
// but the integration-test crate has its own root, so the allows are repeated
// here (e.g. `future_not_send` fires on the insta-guarded async test bodies).
#![allow(clippy::pedantic)]
#![allow(clippy::nursery)]

mod models;
mod requests;
mod tasks;
mod workers;
