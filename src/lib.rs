//! mdmux — a terminal UI for browsing markdown files and rendering them
//! in a [cmux](https://cmux.app) side panel.
//!
//! The binary `mdmux` is a thin wrapper around [`app::App`], the
//! reusable pieces sit in this library so we can unit-test them without a
//! terminal.

pub mod app;
pub mod cmux;
pub mod tree;
pub mod ui;
