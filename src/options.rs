use clap::Parser;

#[derive(Parser)]
pub struct Options {
    pub command: Option<String>,
}
