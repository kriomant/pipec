use append_only_bytes::AppendOnlyBytes;
use std::{
    collections::HashMap, process::ExitStatus
};
use tokio::
    process::{Child, ChildStderr, ChildStdin, ChildStdout}
;
use tui_input::Input;

use crate::id_generator::Id;

pub(crate) struct Execution {
    /// Mapping from stage ID to index of corresponding
    /// stage execution in `pipeline`.
    pub index: HashMap<Id, usize>,

    /// Sequence of commands being executed.
    pub pipeline: Vec<StageExecution>,

    interrupted: bool,
}

impl Execution {
    pub fn new() -> Self {
        Self {
            index: HashMap::new(),
            pipeline: Vec::new(),
            interrupted: false,
        }
    }

    pub fn get_stage(&self, id: Id) -> Option<&StageExecution> {
        self.index.get(&id).cloned().map(|index| &self.pipeline[index])
    }

    pub fn finished(&self) -> bool {
        self.pipeline.iter().all(|stage| stage.finished())
    }

    pub fn interrupt(&mut self) {
        if self.interrupted { return }

        for exec in &mut self.pipeline {
            exec.interrupt();
        }
    }
}

pub(crate) enum ProcessStatus {
    Running(Child),
    Exited(ExitStatus),
}

pub(crate) struct StageExecution {
    pub(crate) command: String,

    pub(crate) status: ProcessStatus,
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,

    /// Number of bytes written to stdin from previous stage's output.
    pub(crate) bytes_written_to_stdin: usize,
    pub(crate) output: AppendOnlyBytes,
}
impl StageExecution {
    pub fn may_reuse_for(&self, command: &str) -> bool {
        if command != self.command {
            return false;
        }

        match self.status {
            ProcessStatus::Running(_) => true,
            ProcessStatus::Exited(status) => status.success(),
        }
    }

    fn interrupt(&mut self) {
        if let ProcessStatus::Running(child) = &mut self.status {
            child.start_kill().unwrap();
        }
    }

    fn finished(&self) -> bool {
        matches!(self.status, ProcessStatus::Exited(_)) && self.stdout.is_none() && self.stderr.is_none()
    }
}

pub(crate) struct Stage {
    // Unique stage ID.
    pub id: Id,

    // Current command entered by user.
    // It may not be the same command with wich `execution` is started.
    // And it's not necessary command which will be executed next.
    pub input: Input,

    pub enabled: bool,
}

impl Stage {
    pub fn new(id: Id) -> Self {
        Self::with_command(id, String::new())
    }

    pub fn with_command(id: Id, command: String) -> Self {
        Self {
            id,
            input: Input::new(command),
            enabled: true
        }
    }
}

pub(crate) struct PendingExecution {
    /// IDs of stages to associate with output of this command.
    /// There are several IDs and not just one because disabled commands
    /// are associated with output of preceeding enabled command.
    pub stage_ids: Vec<Id>,

    /// Command to execute.
    pub command: String,
}
