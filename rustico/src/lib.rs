#![feature(pointer_is_aligned)]

mod assemblyscript;
#[cfg(feature = "repl")]
pub mod repl;
mod service;

pub use service::{Command, Service};
