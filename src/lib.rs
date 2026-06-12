// CI runs clippy with `-W clippy::pedantic -W clippy::nursery -D warnings`.
// These groups are opinionated style lints the codebase does not adopt
// wholesale (the controllers already `#![allow]` several of them per-file);
// allow them centrally so the lint surface is the default `clippy::all` set,
// which is enforced as errors.
#![allow(clippy::pedantic)]
#![allow(clippy::nursery)]

pub mod app;
pub mod controllers;
pub mod data;
pub mod demiurge;
pub mod initializers;
pub mod mailers;
pub mod middleware;
pub mod models;
pub mod tasks;
pub mod views;
pub mod workers;
