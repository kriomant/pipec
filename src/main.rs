#![feature(result_option_map_or_default, box_patterns, import_trait_associated_functions, exit_status_error)]

use append_only_bytes::{AppendOnlyBytes, BytesSlice};
use bstr::ByteSlice;
use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{future::{BoxFuture, OptionFuture}, stream::FuturesUnordered, FutureExt as _, StreamExt as _};
use itertools::Itertools as _;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect, Size},
    style::{Color, Style, Stylize as _},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame, Terminal,
};
use recycle_vec::VecExt;
use std::{
    collections::HashMap, fs::File, io::{self, ErrorKind, Write}, os::unix::process::ExitStatusExt, process::{ExitStatus, Stdio}
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _}, process::{Child, ChildStderr, ChildStdin, ChildStdout, Command}, select,
};
use tui_input::{backend::crossterm::EventHandler, Input};

mod parser;
mod options;
mod id_generator;
mod ui_utils;

use crate::{ui_utils::{status_failed_span, status_killed_span, status_running_span, status_successful_span, status_unknown_span}, options::{Options, PrintOnExit}};
use crate::id_generator::{Id, IdGenerator};

const UNICODE_REPLACEMENT_CODEPOINT: &str = "\u{FFFD}";

struct Execution {
    /// Mapping from stage ID to index of corresponding
    /// stage execution in `pipeline`.
    index: HashMap<Id, usize>,

    /// Sequence of commands being executed.
    pipeline: Vec<StageExecution>,

    interrupted: bool,
}

impl Execution {
    fn new() -> Self {
        Self {
            index: HashMap::new(),
            pipeline: Vec::new(),
            interrupted: false,
        }
    }

    fn get_stage(&self, id: Id) -> Option<&StageExecution> {
        self.index.get(&id).cloned().map(|index| &self.pipeline[index])
    }

    fn finished(&self) -> bool {
        self.pipeline.iter().all(|stage| stage.finished())
    }

    fn interrupt(&mut self) {
        if self.interrupted { return }

        for exec in &mut self.pipeline {
            exec.interrupt();
        }
    }
}

enum ProcessStatus {
    Running(Child),
    Exited(ExitStatus),
}

struct StageExecution {
    command: String,

    status: ProcessStatus,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,

    /// Number of bytes written to stdin from previous stage's output.
    bytes_written_to_stdin: usize,
    output: AppendOnlyBytes,
}
impl StageExecution {
    fn may_reuse_for(&self, command: &str) -> bool {
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

struct Stage {
    // Unique stage ID.
    id: Id,

    // Current command entered by user.
    // It may not be the same command with wich `execution` is started.
    // And it's not necessary command which will be executed next.
    input: Input,

    enabled: bool,
}

impl Stage {
    fn new(id: Id) -> Self {
        Self::with_command(id, String::new())
    }

    fn with_command(id: Id, command: String) -> Self {
        Self {
            id,
            input: Input::new(command),
            enabled: true
        }
    }
}

struct PendingExecution {
    /// IDs of stages to associate with output of this command.
    /// There are several IDs and not just one because disabled commands
    /// are associated with output of preceeding enabled command.
    stage_ids: Vec<Id>,

    /// Command to execute.
    command: String,
}

struct App {
    options: Options,

    id_gen: IdGenerator,

    should_quit: bool,

    // Sequence of commands (stages) edited by user.
    pipeline: Vec<Stage>,

    // Current execution.
    execution: Execution,

    // Pending execution.
    // List of commands to execute when current execution is finished.
    pending_execution: Vec<PendingExecution>,

    // Index of focused pipe in `pipeline`.
    // This is command currently edited by user.
    focused_stage: usize,

    // Index of shown pipe.
    // This is pipe whose output is shown to user.
    shown_stage_index: usize,

    // Caches which hold vector capacity for reuse.
    invalid_line: String,
    lines_cache: Vec<Line<'static>>,
    spans_caches: Vec<Vec<Span<'static>>>,

    /// External pager process.
    external_process: Option<ExternalProcess>,
}

impl App {
    fn new(mut options: Options) -> Result<App, Box<dyn std::error::Error>> {
        let mut id_gen = IdGenerator::new();

        let mut commands = std::mem::take(&mut options.commands);
        if options.parse_commands {
            commands = commands.into_iter()
                .map(|c| {
                    parser::split_pipeline(&c)
                        .map(|cmds| cmds.into_iter().map(|c| c.to_string()).collect::<Vec<_>>())
                })
                .flatten_ok()
                .collect::<Result<Vec<_>, _>>()?;
        };

        let mut pipeline: Vec<_> = commands.into_iter().map(|cmd| Stage::with_command(id_gen.gen_id(), cmd)).collect();
        if pipeline.is_empty() {
            pipeline.push(Stage::new(id_gen.gen_id()));
        }
        let focused_stage = pipeline.len() - 1;
        let shown_stage = focused_stage;

        Ok(App {
            id_gen,
            options,
            should_quit: false,
            pipeline,
            focused_stage,
            shown_stage_index: shown_stage,
            execution: Execution::new(),
            pending_execution: Vec::new(),
            external_process: None,

            invalid_line: String::new(),
            lines_cache: Vec::new(),
            spans_caches: Vec::new(),
        })
    }

    fn render(&mut self, f: &mut Frame) {
        // Output of shown stage is displayed right before it. So shown stage and
        // stages before it are shown above output area and others are shown below.

        // Each stage takes one line.
        let mut constraints: Vec<_> = std::iter::repeat_n(Constraint::Length(1), self.pipeline.len())
            .collect();

        // Output pane takes the rest.
        let shown_stage = &self.pipeline[self.shown_stage_index];
        let show_output = self.execution.index.contains_key(&shown_stage.id);
        if show_output {
            constraints.insert(self.shown_stage_index+1, Constraint::Min(0));
        } else {
            // Show help when there is no output to show.
            constraints.insert(0, Constraint::Fill(1));
        }

        let mut areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(f.area())
            .to_vec();

        if !show_output {
            let help_area = areas.remove(0);
            ui_utils::render_help(f, help_area);
        }

        // Output area
        if show_output {
            let output_area = areas.remove(self.shown_stage_index+1);
            if let Some(shown_stage_exec_index) = self.execution.index.get(&shown_stage.id).cloned() {
                let buf = self.execution.pipeline[shown_stage_exec_index].output.as_bytes();

                // Create text, reusing vector allocations.
                let mut lines = std::mem::take(&mut self.lines_cache).recycle();
                lines.extend(self.spans_caches.drain(..).map(|v| Line { spans: v.recycle(), ..Default::default() }));

                let mut text = Text::from(lines);
                render_binary(buf, output_area.as_size(), &mut text, &mut self.invalid_line);

                f.render_widget(&text, output_area);

                // Save vector allocations for reuse.
                self.spans_caches.extend(text.lines.iter_mut().map(|line| std::mem::take(&mut line.spans).recycle()));
                self.lines_cache = text.lines.recycle();
            }
        }

        let stage_areas = areas;
        for (i, (stage, area)) in self.pipeline.iter().zip(stage_areas.iter()).enumerate() {
            let exec = self.execution.get_stage(stage.id);
            let cursor_pos = render_stage(f, stage, exec, *area, i == self.focused_stage);

            if self.focused_stage == i {
                f.set_cursor_position(cursor_pos);
            }
        }
    }

    fn handle_input(&mut self, event: Event) {
        #[allow(clippy::single_match)]
        match event {
            Event::Key(key) => {
                match key {
                    KeyEvent { code: KeyCode::Char('q'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        self.should_quit = true;
                    }
                    KeyEvent { code: KeyCode::Char('p'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        self.pipeline.insert(self.focused_stage, Stage::new(self.id_gen.gen_id()));
                    }
                    KeyEvent { code: KeyCode::Char('n'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        self.focused_stage += 1;
                        self.pipeline.insert(self.focused_stage, Stage::new(self.id_gen.gen_id()));
                    }
                    KeyEvent { code: KeyCode::Char('d'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        if self.pipeline.len() > 1 {
                            self.pipeline.remove(self.focused_stage);
                            if self.focused_stage != 0 {
                                self.focused_stage -= 1;
                            }

                            if self.shown_stage_index >= self.pipeline.len() {
                                self.shown_stage_index = self.pipeline.len() - 1;
                            }
                        }
                    }
                    KeyEvent { code: KeyCode::Char('x'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        let enabled = &mut self.pipeline[self.focused_stage].enabled;
                        *enabled = !*enabled;
                    }
                    KeyEvent { code: KeyCode::Char('c'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL|KeyModifiers::SHIFT, ..} => {
                        log::info!("hard-terminate executions");
                        {
                            let this = &mut *self;
                            this.execution.interrupt();
                        };
                    }
                    KeyEvent { code: KeyCode::Up, kind: KeyEventKind::Press, modifiers: KeyModifiers::NONE, ..} => {
                        // Move focus to previous stage.
                        if self.focused_stage != 0 {
                            self.focused_stage -= 1;
                        }
                    }
                    KeyEvent { code: KeyCode::Down, kind: KeyEventKind::Press, modifiers: KeyModifiers::NONE, ..} => {
                        // Move focus to next stage.
                        if self.focused_stage < self.pipeline.len() - 1 {
                            self.focused_stage += 1;
                        }
                    }
                    KeyEvent { code: KeyCode::Enter, kind: KeyEventKind::Press, modifiers: KeyModifiers::NONE, ..} => {
                        self.create_pending_execution();
                        self.shown_stage_index = self.focused_stage;
                    }
                    KeyEvent { code: KeyCode::Char(' '), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        self.shown_stage_index = self.focused_stage;
                    }
                    KeyEvent { code: KeyCode::Char('l'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        let _ = self.launch_pager();
                    }
                    KeyEvent { code: KeyCode::Char('v'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        let command = self.pipeline[self.focused_stage].input.value().to_string();
                        let _ = self.launch_editor(command);
                    }
                    _ => {
                        self.pipeline[self.focused_stage].input.handle_event(&event);
                    }
                }
            }
            _ => {}
        }
    }

    fn create_pending_execution(&mut self) {
        self.pending_execution.clear();

        // Create
        for stage in &self.pipeline {
            if stage.enabled {
                self.pending_execution.push(PendingExecution {
                    stage_ids: vec![stage.id],
                    command: stage.input.value().to_string()
                });
            } else {
                // Disabled commands are attached to preceeding enabled
                // command and show it's output.
                // Leading disabled commands are completely ignored.
                if let Some(pending_stage) = self.pending_execution.last_mut() {
                    pending_stage.stage_ids.push(stage.id);
                }
            }
        }

        self.execution.interrupt();
    }

    /// Start pending execution if current one is finished.
    fn execute_pending(&mut self) {
        assert!(self.execution.finished());

        let pending_commands = std::mem::take(&mut self.pending_execution);
        self.execution.index.clear();

        // Try to reuse parts of last execution.
        // Execution stage may be reused if it's command matches one in pending execution,
        // it is still running or successfully finished.
        // Since execution stage is (intentionally) not directly tied to visible stage,
        // but only though index, it may be reused event for another visible stage.
        let number_of_steps_to_reuse = pending_commands.iter().zip(self.execution.pipeline.iter())
            .take_while(|(pending, old)| old.may_reuse_for(&pending.command))
            .count();
        self.execution.pipeline.truncate(number_of_steps_to_reuse);
        for (i, exec) in pending_commands.iter().enumerate().take(number_of_steps_to_reuse) {
            for &stage_id in &exec.stage_ids {
                self.execution.index.insert(stage_id, i);
            }
        }

        // Now add new (non-reused) stages.
        for (i, exec) in pending_commands.into_iter().enumerate().skip(number_of_steps_to_reuse) {
            self.execution.pipeline.push(self.start_command(exec.command, i != 0).unwrap());
            for &stage_id in &exec.stage_ids {
                self.execution.index.insert(stage_id, self.execution.pipeline.len()-1);
            }
        }

        // Validate indices in `index`, they all must point to valid execution pipeline stage.
        assert!(self.execution.index.values().all(|&v| v < self.execution.pipeline.len()),
            "pipeline len: {}, index: {:?}, reused: {}",
            self.execution.pipeline.len(), self.execution.index, number_of_steps_to_reuse);
    }

    fn handle_process_terminated(&mut self, i: usize, exit_status: ExitStatus) -> std::io::Result<()> {
        log::info!("stage {i}: teminated: {exit_status:?}");
        let stage = &mut self.execution.pipeline[i];

        assert!(matches!(stage.status, ProcessStatus::Running(_)));
        log::info!("stage {i}: finished");
        stage.status = ProcessStatus::Exited(exit_status);

        Ok(())
    }

    fn handle_stdin(&mut self, i: usize, bytes_written: usize) {
        log::info!("stage {i}: written {bytes_written} bytes to stdin");
        let stage = &mut self.execution.pipeline[i];

        if bytes_written == 0 {
            stage.stdin = None;
            return;
        }

        stage.bytes_written_to_stdin += bytes_written;
        log::debug!("stage {}: {} bytes written to stdin", i, stage.bytes_written_to_stdin);

        let total_written = stage.bytes_written_to_stdin;
        let should_close_stdin = {
            let prev_exec = &mut self.execution.pipeline[i-1];
            total_written == prev_exec.output.len() && prev_exec.stdout.is_none()
        };

        let stage = &mut self.execution.pipeline[i];
        if should_close_stdin {
            stage.stdin = None;
        }
    }

    fn handle_stdout(&mut self, i: usize, buf: &[u8]) {
        log::info!("stage {}: read {} bytes from stdout", i, buf.len());
        let stage = &mut self.execution.pipeline[i];

        if !buf.is_empty() {
            stage.output.push_slice(buf);
            return;
        }

        // Stdout is closed.
        stage.stdout = None;

        // Close stdin of next stage, if all data are already written.
        let output_len = stage.output.len();
        if i < self.pipeline.len() - 1 {
            let next_stage = &mut self.execution.pipeline[i+1];
            if next_stage.bytes_written_to_stdin == output_len {
                next_stage.stdin = None;
            }
        }

        // Close external pager input.
        if i == self.shown_stage_index
            && let Some(ExternalProcess::Pager(pager)) = self.external_process.as_mut()
        {
            pager.stdin = None;
        }
    }

    fn handle_stderr(&mut self, i: usize, buf: &[u8]) {
        log::info!("stage {}: read {} bytes from stderr", i, buf.len());
        let stage = &mut self.execution.pipeline[i];

        if !buf.is_empty() {
            stage.output.push_slice(buf);
            return;
        }

        // Stderr is closed.
        stage.stderr = None;

        // Close stdin of next stage, if all data are already written.
        let output_len = stage.output.len();
        if i < self.pipeline.len() - 1 {
            let next_stage = &mut self.execution.pipeline[i+1];
            if next_stage.bytes_written_to_stdin == output_len {
                next_stage.stdin = None;
            }
        }
    }

    fn handle_pager_stdin(&mut self, bytes_written: usize) {
        let Some(ExternalProcess::Pager(pager)) = self.external_process.as_mut() else { return };

        if bytes_written == 0 {
            pager.stdin = None;
            return;
        }

        pager.bytes_written += bytes_written;
        log::debug!("pager: {} bytes written to stdin", pager.bytes_written);

        let total_written = pager.bytes_written;
        let should_close_stdin = {
            let shown_stage_id = self.pipeline[self.shown_stage_index].id;
            let Some(&exec_idx) = self.execution.index.get(&shown_stage_id) else { return };
            let exec = &self.execution.pipeline[exec_idx];
            total_written == exec.output.len() && exec.stdout.is_none()
        };

        if should_close_stdin {
            pager.stdin = None;
        }
    }

    fn handle_pager_exit(&mut self, exit_status: ExitStatus, terminal: &mut Terminal<CrosstermBackend<io::Stderr>>) -> std::io::Result<()> {
        log::info!("pager: teminated: {exit_status:?}");
        assert!(matches!(self.external_process, Some(ExternalProcess::Pager(_))));
        self.external_process = None;

        execute!(
            io::stderr(),
            EnterAlternateScreen
        )?;
        enable_raw_mode()?;

        // It's not to clear terminal window, it's to make terminal acknowledge that
        // it is invalidated and redraw it.
        terminal.clear()?;

        Ok(())
    }

    fn handle_editor_exit(&mut self, command: Option<String>, terminal: &mut Terminal<CrosstermBackend<io::Stderr>>) -> std::io::Result<()> {
        log::info!("editor: success: {}", command.is_some());
        assert!(matches!(self.external_process, Some(ExternalProcess::Editor(_))));
        self.external_process = None;

        terminal.clear()?;

        if let Some(command) = command {
            let stage = &mut self.pipeline[self.focused_stage];
            stage.input = Input::new(command);
        }

        Ok(())
    }

    fn start_command(&self, command: String, stdin: bool) -> std::io::Result<StageExecution> {
        let mut cmd = Command::new(self.options.resolve_shell());
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

    fn launch_pager(&mut self) -> std::io::Result<()> {
        use tokio::process::Command;
        use std::process::Stdio;

        assert!(self.external_process.is_none());

        // Find stage execution index for shown stage.
        let shown_id = self.pipeline[self.shown_stage_index].id;
        if !self.execution.index.contains_key(&shown_id) {
            return Ok(());
        }

        execute!(
            io::stderr(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        disable_raw_mode()?;

        let pager = self.options.resolve_pager();
        let mut cmd = Command::new(pager);
        cmd.stdin(Stdio::piped());
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take();

        self.external_process = Some(ExternalProcess::Pager(Pager {
            process: child,
            stdin,
            bytes_written: 0,
        }));

        Ok(())
    }

    fn launch_editor(&mut self, mut contents: String) -> std::io::Result<()> {
        assert!(self.external_process.is_none());

        let editor = self.options.resolve_editor().into_owned();

        let edit = async move {
            let mut file = async_tempfile::TempFile::new().await?;
            log::debug!("temporary file for editor: {}", file.file_path().to_string_lossy());
            log::trace!("write command: {contents}");
            file.write_all(contents.as_bytes()).await?;
            contents.clear();

            execute!(
                io::stderr(),
                LeaveAlternateScreen,
                DisableMouseCapture
            )?;
            disable_raw_mode()?;

            let mut cmd = tokio::process::Command::new(editor);
            cmd.arg(file.file_path());
            log::debug!("start editor: {cmd:?}");
            cmd.spawn()?.wait().await?.exit_ok()?;

            enable_raw_mode()?;
            execute!(
                io::stderr(),
                EnterAlternateScreen
            )?;

            let mut file = file.open_ro().await?;
            file.read_to_string(&mut contents).await?;
            log::trace!("editor: read contents: {contents}");

            // Cleanup contents
            contents = contents.trim().replace('\n', " ").to_string();

            Ok::<_, Box<dyn std::error::Error>>(contents)
        };

        self.external_process = Some(ExternalProcess::Editor(async {
            edit.await.ok()
        }.boxed()));

        Ok(())
    }
}

/// Renders stage into given area.
/// Returns cursor position.
fn render_stage(frame: &mut Frame, stage: &Stage, exec: Option<&StageExecution>, area: Rect, focused: bool) -> Position {
    let [marker_area, command_area, status_area] = Layout
        ::horizontal([
            Constraint::Min(1),
            Constraint::Percentage(100),
            Constraint::Min(1),
        ])
        .spacing(1)
        .areas(area);

    // Prompt sign
    let marker_color = if focused { Color::Green } else { Color::Gray };
    frame.render_widget(Span::styled("❯", Style::default().fg(marker_color)), marker_area);

    // Draw command.
    // Commands changed from last execution are highlighted with bold.
    let mut command_style = Style::default();
    if !stage.enabled {
        command_style = command_style.crossed_out();
    }
    if exec.is_none_or(|e| e.command != stage.input.value()) {
        command_style = command_style.bold()
    }
    let scroll = stage.input.visual_scroll(command_area.width as usize - 1);
    let command = Paragraph::new(Span::styled(stage.input.value(), command_style))
        .scroll((0, scroll as u16));
    frame.render_widget(command, command_area);

    // Status indicator
    let status_span = match exec {
        None => Span::raw(" "),  // Command is running
        Some(ex) => match ex.status {
            ProcessStatus::Running(_) => status_running_span(),
            ProcessStatus::Exited(status) => {
                if status.success() {
                    status_successful_span()
                } else if status.code().is_some() {
                    status_failed_span()
                } else if status.signal().is_some() {
                    status_killed_span()
                } else {
                    status_unknown_span()
                }
            }
        }
    };
    let status = Paragraph::new(Line::from(vec![status_span]))
        .alignment(Alignment::Right);
    frame.render_widget(status, status_area);

    let cursor_offset = stage.input.visual_cursor().max(scroll) - scroll;
    Position::new(command_area.x + cursor_offset as u16, command_area.y)
}

enum UiEvent {
    Term(crossterm::event::Event),
    Stage(usize, StageEvent),
    Pager(PagerEvent),
    EditorFinished(Option<String>),
}

enum PagerEvent {
    Exit(ExitStatus),
    Stdin(usize),
}

enum StageEvent {
    Exit(ExitStatus),
    Stdin(usize),
    Stdout(Vec<u8>, usize),
    Stderr(Vec<u8>, usize),
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    mut app: App,
) -> io::Result<App> {
    let mut term_event_reader = crossterm::event::EventStream::new();

    loop {
        let external_program_active = app.external_process.is_some();

        if !external_program_active {
            terminal.draw(|f| app.render(f))?;
        }

        let event = {
            let mut exit_futures = FuturesUnordered::new();
            let mut stdin_futures = FuturesUnordered::new();
            let mut stdout_futures = FuturesUnordered::new();
            let mut stderr_futures = FuturesUnordered::new();

            let mut pager_stdin_future: OptionFuture<_> = None.into();
            let mut pager_exit_future: OptionFuture<_> = None.into();

            let mut editor_future: OptionFuture<_> = None.into();

            let shown_exec_idx = app.execution.index.get(&app.pipeline[app.shown_stage_index].id).cloned();

            let mut pager_data = None;
            if let Some(p) = app.external_process.as_mut() {
                match p {
                    ExternalProcess::Pager(pager) => {
                        pager_exit_future = Some(pager.process.wait()).into();
                        if let Some(stdin) = &mut pager.stdin {
                            pager_data = Some((pager.bytes_written, stdin));
                        }
                    }
                    ExternalProcess::Editor(f) => {
                        editor_future = Some(f).into();
                    }
                }
            }

            app.execution.pipeline.iter_mut().enumerate().fold(None, |prev_stdout: Option<BytesSlice>, (i, exec)| {
                if let ProcessStatus::Running(child) = &mut exec.status {
                    exit_futures.push(child.wait().map(move |status| (i, status)));
                }

                if let (Some(stdin), Some(prev_stdout)) = (&mut exec.stdin, prev_stdout) {
                    let bytes_written_to_stdin = exec.bytes_written_to_stdin;
                    if bytes_written_to_stdin < prev_stdout.len() {
                        log::debug!("stage {}: {} of {} previous stage output bytes are written to stdin", i, exec.bytes_written_to_stdin, prev_stdout.len());
                        stdin_futures.push(async move {
                            log::debug!("stage {}: schedule writing {} bytes", i, &prev_stdout.as_bytes()[bytes_written_to_stdin..].len());
                            let buf = &prev_stdout.as_bytes()[bytes_written_to_stdin..];
                            let res = stdin.write(buf).await;
                            (i, res)
                        });
                    }
                }
                if let Some(stdout) = &mut exec.stdout {
                    stdout_futures.push(async move {
                        let mut buf = vec![0; 1024];
                        let res = stdout.read(&mut buf).await;
                        (i, res, buf)
                    });
                }
                if let Some(stderr) = &mut exec.stderr {
                    stderr_futures.push(async move {
                        let mut buf = vec![0; 1024];
                        let res = stderr.read(&mut buf).await;
                        (i, res, buf)
                    });
                }

                let output = exec.output.slice(..);

                if shown_exec_idx == Some(i)
                    && let Some((written, pager_stdin)) = pager_data.take()
                    && written < exec.output.len()
                {
                    pager_stdin_future = Some({
                        let output = output.clone();
                        async move {
                            let buf = &output.as_bytes()[written..];
                            let res = pager_stdin.write(buf).await;
                            let _ = pager_stdin.flush().await;
                            res
                        }
                    }).into();
                }

                Some(output)
            });

            select! {
                result = term_event_reader.next(), if !external_program_active => {
                    match result {
                        Some(Ok(event)) => UiEvent::Term(event),
                        _ => break
                    }
                }
                Some(cmd) = editor_future => {
                    UiEvent::EditorFinished(cmd)
                }
                Some((i, status)) = exit_futures.next() => {
                    UiEvent::Stage(i, StageEvent::Exit(status?))
                }
                Some((i, res)) = stdin_futures.next() => {
                    let n = match res {
                        Ok(n) => n,
                        // BrokenPipe is normal situation when process is terminated,
                        // handle it same way as if stdin was properly closed.
                        Err(err) if err.kind() == ErrorKind::BrokenPipe => 0,
                        err => err?,
                    };
                    UiEvent::Stage(i, StageEvent::Stdin(n))
                }
                Some((i, res, buf)) = stdout_futures.next() => {
                    let bytes_read = res?;
                    UiEvent::Stage(i, StageEvent::Stdout(buf, bytes_read))
                }
                Some((i, res, buf)) = stderr_futures.next() => {
                    let bytes_read = res?;
                    UiEvent::Stage(i, StageEvent::Stderr(buf, bytes_read))
                }
                Some(res) = pager_stdin_future => {
                    let n = match res {
                        Ok(n) => n,
                        // BrokenPipe is normal situation when process is terminated,
                        // handle it same way as if stdin was properly closed.
                        Err(err) if err.kind() == ErrorKind::BrokenPipe => 0,
                        err => err?,
                    };
                    UiEvent::Pager(PagerEvent::Stdin(n))
                }
                Some(res) = pager_exit_future => {
                    UiEvent::Pager(PagerEvent::Exit(res?))
                }
            }
        };

        match event {
            UiEvent::Term(event) => app.handle_input(event),
            UiEvent::Stage(i, StageEvent::Exit(status)) => app.handle_process_terminated(i, status).unwrap(),
            UiEvent::Stage(i, StageEvent::Stdin(n)) => app.handle_stdin(i, n),
            UiEvent::Stage(i, StageEvent::Stdout(buf, n)) => app.handle_stdout(i, &buf[..n]),
            UiEvent::Stage(i, StageEvent::Stderr(buf, n)) => app.handle_stderr(i, &buf[..n]),
            UiEvent::Pager(PagerEvent::Stdin(n)) => app.handle_pager_stdin(n),
            UiEvent::Pager(PagerEvent::Exit(status)) => app.handle_pager_exit(status, terminal)?,
            UiEvent::EditorFinished(cmd) => app.handle_editor_exit(cmd, terminal)?,
        }

        if app.should_quit {
            break;
        }

        if !app.pending_execution.is_empty() && app.execution.finished() {
            app.execute_pending();
        }
    }

    Ok(app)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::parse();

    if let Some(logging) = &options.logging {
        let log_writer = File::create(options.log_file.as_ref().unwrap())?;
        env_logger::Builder::new()
            .parse_filters(logging)
            .target(env_logger::Target::Pipe(Box::new(log_writer)))
            .try_init()?;
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut writer = io::stderr();
    execute!(writer, EnterAlternateScreen)?;

    // Panic handler to reset terminal
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        disable_raw_mode().unwrap();
        execute!(
            io::stderr(),
            LeaveAlternateScreen,
            DisableMouseCapture
        ).unwrap();

        prev_hook(info);
    }));

    let backend = CrosstermBackend::new(writer);
    let mut terminal = Terminal::new(backend)?;

    // Create app and run it
    let app = App::new(options)?;
    let res = run_app(&mut terminal, app).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    match res {
        Ok(app) => {
            match app.options.print_on_exit {
                PrintOnExit::Pipeline => {
                    for (i, stage) in app.pipeline.iter().enumerate() {
                        if i != 0 {
                            print!(" | ");
                        }
                        print!("{}", stage.input.value());
                    }
                    println!();
                }
                PrintOnExit::Output => {
                    if let Some(stage) = app.execution.pipeline.last() {
                        std::io::stdout().write_all(stage.output.as_bytes())?;
                    }
                }
                PrintOnExit::Nothing => {}
            }
        }
        Err(err) => {
            eprintln!("{err:?}");
        }
    }

    Ok(())
}

/// Renders binary data into given `Text`.
/// Valid UTF8 graphemes are rendered as is, invalid ones are replaced with
/// Unicode Replacement codepoints.
///
/// This function is designed to reuse existing text. If size of area
/// and data haven't changed since last render, then no new allocations should occur.
fn render_binary<'t, 'i: 't, 'b: 't>(
    buf: &'b [u8], area: Size, text: &mut Text<'t>, invalid_slice: &'i mut String
) {
    // Line full of Unicode Replacement codepoint, which is used to represent
    // invalid bytes. We slice it to represent any number of such bytes in line
    // without requiring additional memory.
    if invalid_slice.len() < area.width as usize {
        *invalid_slice = UNICODE_REPLACEMENT_CODEPOINT.repeat(area.width as usize);
    }

    let output = bstr::BStr::new(buf);

    let mut current_line = 0;
    text.lines.resize(area.height as usize, Line::default());
    for line in &mut text.lines {
        line.spans.clear();
    }

    'outer: for (newline, graphemes) in output.grapheme_indices()
        .chunk_by(|(_, _, str)| *str == "\n")
        .into_iter()
    {
        if newline {
            current_line += graphemes.count();
            if current_line >= text.lines.len() {
                break 'outer;
            }
            continue;
        }

        for line in graphemes.chunks(area.width as usize).into_iter() {
            for (invalid, mut span) in line.chunk_by(|(_, _, g)| *g == UNICODE_REPLACEMENT_CODEPOINT).into_iter() {
                let span = if invalid {
                    Span::from(&invalid_slice[..UNICODE_REPLACEMENT_CODEPOINT.len()*span.count()])
                        .light_red()
                } else {
                    let first = span.next().unwrap();
                    let last = span.last().unwrap_or(first);
                    let (start, end) = (first.0, last.1);
                    Span::from(str::from_utf8(&buf[start..end]).unwrap())
                };

                text.lines[current_line].spans.push(span);
            }
        }
    }
}

/// Struct to represent external pager process and its stdin.
struct Pager {
    process: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    bytes_written: usize,
}

enum ExternalProcess {
    Pager(Pager),
    Editor(BoxFuture<'static, Option<String>>),
}

#[cfg(test)]
mod tests {
    use ratatui::{layout::Size, style::Stylize, text::{Line, Span, Text}};
    use std::default::Default::default;

    use crate::{render_binary, UNICODE_REPLACEMENT_CODEPOINT};

    #[test]
    fn test_render_binary_valid_utf8() {
        let mut invalid_slice = String::new();
        let mut text = Text::default();
        render_binary(b"abcdef", Size::new(10, 1), &mut text, &mut invalid_slice);
        assert_eq!(
            text,
            Text {
                lines: vec![
                    Line::from("abcdef"),
                ],
                ..default()
            }
        );
    }

    #[test]
    fn test_render_binary_invalid_utf8() {
        let mut invalid_slice = String::new();
        let mut text = Text::default();
        render_binary(b"abc\xffdef", Size::new(10, 1), &mut text, &mut invalid_slice);
        assert_eq!(
            text,
            Text::from(vec![
                Line::default().spans(vec![
                    Span::from("abc"),
                    Span::from(UNICODE_REPLACEMENT_CODEPOINT).light_red(),
                    Span::from("def"),
                ]),
            ])
        );
    }

    #[test]
    fn test_render_binary_renders_sequential_invalid_bytes_as_single_span() {
        let mut invalid_slice = String::new();
        let mut text = Text::default();
        render_binary(b"abc\xff\xffdef", Size::new(10, 1), &mut text, &mut invalid_slice);
        assert_eq!(
            text,
            Text::from(vec![
                Line::default().spans(vec![
                    Span::from("abc"),
                    Span::from(UNICODE_REPLACEMENT_CODEPOINT.repeat(2)).light_red(),
                    Span::from("def"),
                ]),
            ])
        );
    }

    /// Tests that lines are allocated for whole area, even if there is less
    /// real lines in buffer.
    #[test]
    fn test_render_binary_allocates_lines_for_whole_area() {
        let mut invalid_slice = String::new();
        let mut text = Text::default();
        render_binary(b"abc\xffdef", Size::new(10, 2), &mut text, &mut invalid_slice);
        assert_eq!(
            text,
            // We render just one line, but are has two rows.
            Text::from(vec![
                Line::default().spans(vec![
                    Span::from("abc"),
                    Span::from(UNICODE_REPLACEMENT_CODEPOINT).light_red(),
                    Span::from("def"),
                ]),
                Line::default(),
            ])
        );
    }

    /// Tests that number of rendered lines is limited by area height.
    #[test]
    fn test_render_binary_number_of_lines_limited_by_area_height() {
        let mut invalid_slice = String::new();
        let mut text = Text::default();
        render_binary(b"abc\ndef\nghi", Size::new(10, 2), &mut text, &mut invalid_slice);
        assert_eq!(
            text,
            // We render just one line, but are has two rows.
            Text::from(vec![
                Line::from("abc"),
                Line::from("def"),
            ])
        );
    }
}
