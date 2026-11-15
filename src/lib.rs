//! DicingSplicing as a library: the editor, and ds-tool, which changes its projects from the command
//! line for scripts and AI agents. The two programs only start what is here: `main.rs` the editor,
//! `bin/ds-tool.rs` ds-tool.

pub mod app;
pub mod cli;
mod core;
mod media;
mod ui;
