use std::{ffi::OsString, path::PathBuf};

use clap::{Parser, ValueEnum};

#[derive(ValueEnum, Clone, Copy)]
pub enum PrintOnExit {
    Nothing,
    Pipeline,
    Output,
}

#[derive(Parser)]
pub struct Options {
    pub commands: Vec<String>,

    /// Parse shell pipeline commands provided as arguments
    /// into individual pipe commands.
    #[arg(long)]
    pub parse_commands: bool,

    /// What to print on program exit.
    #[arg(long, default_value="nothing")]
    pub print_on_exit: PrintOnExit,

    #[arg(long)]
    pub shell: Option<OsString>,

    #[arg(long, requires="log_file")]
    pub logging: Option<String>,
    #[arg(long)]
    pub log_file: Option<PathBuf>,
}
