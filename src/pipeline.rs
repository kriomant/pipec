use append_only_bytes::AppendOnlyBytes;
use std::{
    collections::HashMap, ffi::OsStr, process::{ExitStatus, Stdio}
};
use tokio::
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command}
;
use tui_input::Input;

use crate::id_generator::Id;

#[derive(Default)]
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

pub(crate) struct PendingStage {
    /// IDs of stages to associate with output of this command.
    /// There are several IDs and not just one because disabled commands
    /// are associated with output of preceeding enabled command.
    pub stage_ids: Vec<Id>,

    /// Command to execute.
    pub command: String,
}

pub(crate) struct PendingExecution {
    pub pipeline: Vec<PendingStage>,
}
impl PendingExecution {
    pub fn execute(self, shell: &OsStr, mut execution_to_reuse: Execution) -> Execution {
        assert!(execution_to_reuse.finished());

        // Try to reuse parts of last execution.
        // Execution stage may be reused if it's command matches one in pending execution,
        // it is still running or successfully finished.
        // Since execution stage is (intentionally) not directly tied to visible stage,
        // but only though index, it may be reused event for another visible stage.
        let number_of_steps_to_reuse = self.pipeline.iter().zip(execution_to_reuse.pipeline.iter())
            .take_while(|(pending, old)| old.may_reuse_for(&pending.command))
            .count();
        execution_to_reuse.pipeline.truncate(number_of_steps_to_reuse);

        execution_to_reuse.index.clear();
        for (i, exec) in self.pipeline.iter().enumerate().take(number_of_steps_to_reuse) {
            for &stage_id in &exec.stage_ids {
                execution_to_reuse.index.insert(stage_id, i);
            }
        }

        // Now add new (non-reused) stages.
        for (i, exec) in self.pipeline.into_iter().enumerate().skip(number_of_steps_to_reuse) {
            execution_to_reuse.pipeline.push(start_command(shell, exec.command, i != 0).unwrap());
            for &stage_id in &exec.stage_ids {
                execution_to_reuse.index.insert(stage_id, execution_to_reuse.pipeline.len()-1);
            }
        }

        // Validate indices in `index`, they all must point to valid execution pipeline stage.
        assert!(execution_to_reuse.index.values().all(|&v| v < execution_to_reuse.pipeline.len()),
            "pipeline len: {}, index: {:?}, reused: {}",
            execution_to_reuse.pipeline.len(), execution_to_reuse.index, number_of_steps_to_reuse);

        execution_to_reuse
    }
}

pub(crate) fn start_command(shell: &OsStr, command: String, stdin: bool) -> std::io::Result<StageExecution> {
    let mut cmd = Command::new(shell);
    if cfg!(target_os = "windows") {
        cmd.args(["/C", &command]);
    } else {
        cmd.args(["-c", &command]);
    };

    log::info!("start command: {cmd:?}");
    let mut child = cmd
        .kill_on_drop(true)
        .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    assert!(stdout.is_some() && stderr.is_some());

    Ok(StageExecution {
        command,
        status: ProcessStatus::Running(child),
        stdin,
        stdout,
        stderr,
        bytes_written_to_stdin: 0,
        output: AppendOnlyBytes::new(),
    })
}
