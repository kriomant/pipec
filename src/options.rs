use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
pub struct Options {
    pub command: Option<String>,

    #[arg(long, requires="log_file")]
    pub logging: Option<String>,
    #[arg(long)]
    pub log_file: Option<PathBuf>,
}
