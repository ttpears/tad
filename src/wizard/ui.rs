//! Ratatui state machine and render loop for the wizard.

#![allow(dead_code)]

use anyhow::Result;

#[derive(Debug, Clone, Copy)]
pub enum Entry {
    FirstLaunch,
    Config,
}

pub fn run(_entry: Entry) -> Result<()> {
    // Filled in by Task 10.
    Ok(())
}
