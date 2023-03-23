#![feature(pointer_is_aligned)]

#[cfg(feature = "repl")]
pub mod repl;
mod service;
mod assemblyscript;

pub use service::{Command, Service};
