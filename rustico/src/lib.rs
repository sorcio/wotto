#![feature(pointer_is_aligned)]

mod assemblyscript;
#[cfg(feature = "repl")]
pub mod repl;
mod runtime;
mod service;
mod webload;

pub use service::{Command, Error, Service};
