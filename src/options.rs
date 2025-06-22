use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
pub struct Options {
    pub commands: Vec<String>,

    /// Parse shell pipeline commands provided as arguments
    /// into individual pipe commands.
    #[arg(long)]
    pub parse_commands: bool,

    #[arg(long)]
    pub print_command: bool,

    #[arg(long, requires="log_file")]
    pub logging: Option<String>,
    #[arg(long)]
    pub log_file: Option<PathBuf>,
}
