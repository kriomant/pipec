use std::{borrow::Cow, ffi::{OsStr, OsString}, path::PathBuf};

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

    /// Shell to use. Default: $SHELL, `/bin/sh`.
    #[arg(long)]
    pub shell: Option<OsString>,

    /// Pager program to use. Default: $PAGER, `less`.
    #[arg(long)]
    pub pager: Option<OsString>,

    /// Editor program to use. Default: $VISUAL, $EDITOR, `vim`.
    #[arg(long)]
    pub editor: Option<OsString>,

    #[arg(long, requires="log_file")]
    pub logging: Option<String>,
    #[arg(long)]
    pub log_file: Option<PathBuf>,
}

impl Options {
    pub fn resolve_shell(&self) -> Cow<'_, OsStr> {
        self.shell.as_deref()
            .map(Cow::from)
            .or_else(|| std::env::var_os("SHELL").map(Cow::from))
            .unwrap_or(Cow::from(OsString::from("/bin/sh")))
    }

    pub fn resolve_pager(&self) -> Cow<'_, OsStr> {
        self.pager.as_deref()
            .map(Cow::from)
            .or_else(|| std::env::var_os("PAGER").map(Cow::from))
            .unwrap_or(Cow::from(OsString::from("less")))
    }

    pub fn resolve_editor(&self) -> Cow<'_, OsStr> {
        self.pager.as_deref()
            .map(Cow::from)
            .or_else(|| std::env::var_os("VISUAL").map(Cow::from))
            .or_else(|| std::env::var_os("EDITOR").map(Cow::from))
            .unwrap_or(Cow::from(OsString::from("vim")))
    }
}
