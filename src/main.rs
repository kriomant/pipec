#![feature(result_option_map_or_default, box_patterns, import_trait_associated_functions, exit_status_error)]

use append_only_bytes::BytesSlice;
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
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame, Terminal,
};
use recycle_vec::VecExt;
use std::{
    borrow::Cow, fs::File, io::{self, ErrorKind, Write}, os::unix::process::ExitStatusExt as _, process::ExitStatus
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _}, select,
};
use tui_input::{backend::crossterm::EventHandler, Input};

mod parser;
mod options;
mod id_generator;
mod pipeline;
mod ui;

use crate::{options::{Options, PrintOnExit}, pipeline::{Execution, PendingExecution, PendingStage, ProcessStatus, Stage, StageExecution}, ui::{action::{Action, CopySource, QuitAction}, highlight::highlight_command_to_spans, popup::KeysPopup, utils::{status_failed_span, status_killed_span, status_running_span, status_successful_span, status_unknown_span}}};
use crate::id_generator::IdGenerator;

enum QuitResult {
    Success,
    Cancel,
    Pipeline(Vec<String>),
    Output(BytesSlice),
}

enum ExecutionMode {
    All,
    TillFocused,
}

struct App {
    options: Options,

    id_gen: IdGenerator,

    should_quit: Option<QuitResult>,

    // Sequence of commands (stages) edited by user.
    pipeline: Vec<Stage>,

    // Current execution.
    execution: Execution,

    /// Pending execution.
    /// List of commands to execute when current execution is finished.
    pending_execution: Option<PendingExecution>,

    /// Whether current execution may be reused for next one.
    can_reuse_execution: bool,

    // Index of focused pipe in `pipeline`.
    // This is command currently edited by user.
    focused_stage: usize,

    // Index of shown pipe.
    // This is pipe whose output is shown to user.
    shown_stage_index: usize,

    // Caches which hold vector capacity for reuse.
    output_cache: OutputRenderCache,
    eoutput_cache: OutputRenderCache,

    popup: Option<KeysPopup>,

    /// External pager process.
    external_process: Option<ExternalProcess>,

    clipboard: arboard::Clipboard,
}

#[derive(Default)]
struct OutputRenderCache {
    invalid_line: String,
    lines_cache: Vec<Line<'static>>,
    spans_caches: Vec<Vec<Span<'static>>>,
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

        let mut app = App {
            id_gen,
            options,
            should_quit: None,
            pipeline,
            focused_stage,
            shown_stage_index: shown_stage,
            execution: Execution::default(),
            pending_execution: None,
            can_reuse_execution: true,
            popup: None,
            external_process: None,
            clipboard: arboard::Clipboard::new()?,

            output_cache: Default::default(),
            eoutput_cache: Default::default(),
        };

        if app.options.execute_on_start {
            app.focused_stage = app.pipeline.len()-1;
            app.shown_stage_index = app.focused_stage;
            app.create_pending_execution(ExecutionMode::All);
            app.execute_pending();
        }

        Ok(app)
    }

    fn render(&mut self, f: &mut Frame) {
        // Output of shown stage is displayed right before it. So shown stage and
        // stages before it are shown above output area and others are shown below.

        // Each stage takes one line.
        let mut constraints: Vec<_> = std::iter::repeat_n(Constraint::Length(1), self.pipeline.len())
            .collect();

        // Decide how to use remaining space.
        let mut show_output = false;
        let mut show_eoutput = false;
        let shown_stage = &self.pipeline[self.shown_stage_index];
        if let Some(exec_stage) = self.execution.get_stage(shown_stage.id) {
            // Definitely show stderr if there is anything to show.
            show_eoutput = !exec_stage.eoutput.is_empty();
            // Show stdout if there is anything to show or if stderr is empty.
            show_output = !exec_stage.output.is_empty() || !show_eoutput;
            assert!(show_output || show_eoutput);
        }

        if show_eoutput {
            constraints.insert(self.shown_stage_index+1, Constraint::Fill(1));
        }
        if show_output {
            constraints.insert(self.shown_stage_index+1, Constraint::Fill(1));
        }
        // If there is nothing to show, show help.
        let show_help = !show_output && !show_eoutput;
        if show_help {
            // Show help when there is no output to show.
            constraints.insert(0, Constraint::Fill(1));
        }

        let mut areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(f.area())
            .to_vec();

        if show_help {
            let help_area = areas.remove(0);
            if self.popup.is_none() {
                ui::utils::render_help(f, help_area);
            }
        }

        // Output area
        if show_output {
            let output_area = areas.remove(self.shown_stage_index+1);
            if let Some(shown_stage_exec_index) = self.execution.index.get(&shown_stage.id).cloned() {
                let buf = self.execution.pipeline[shown_stage_exec_index].output.as_bytes();
                render_bytes(f, buf, output_area, Style::default(), &mut self.output_cache);
            }
        }

        // Stderr output area
        if show_eoutput {
            let output_area = areas.remove(self.shown_stage_index+1);
            if let Some(shown_stage_exec_index) = self.execution.index.get(&shown_stage.id).cloned() {
                let buf = self.execution.pipeline[shown_stage_exec_index].eoutput.as_bytes();
                render_bytes(f, buf, output_area, Style::default().fg(Color::Red), &mut self.eoutput_cache);
            }
        }

        let stage_areas = areas;
        for (i, (stage, area)) in self.pipeline.iter().zip(stage_areas.iter()).enumerate() {
            let exec = self.execution.get_stage(stage.id);
            let cursor_pos = render_stage(f, stage, exec, *area, i == self.focused_stage, &self.options);

            if self.focused_stage == i {
                f.set_cursor_position(cursor_pos);
            }
        }

        if let Some(popup) = &self.popup {
            popup.render(f, f.area());
        }
    }

    fn handle_input(&mut self, event: Event) {
        let action = self.get_action_for_event(event);
        if let Some(action) = action {
            self.handle_action(action);
        }
    }

    fn get_action_for_event(&mut self, event: Event) -> Option<Action> {
        if let Some(popup) = &mut self.popup {
            if let Event::Key(key) = event
                && key.code == KeyCode::Esc
                && key.modifiers.is_empty()
                && key.is_press()
            {
                self.popup = None;
                return None;
            }

            let action = popup.get_action(event);
            if action.is_some() {
                self.popup = None;
            }
            return action;
        }

        #[allow(clippy::single_match)]
        match event {
            Event::Key(key) => {
                match key {
                    KeyEvent { code: KeyCode::Char('q'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        let keys = match self.options.print_on_exit {
                            PrintOnExit::Nothing => return Some(Action::Quit(QuitAction::Succeed)),
                            PrintOnExit::Ask => vec![
                                (KeyModifiers::empty(), KeyCode::Char('q'), "Quit without printing", Action::Quit(QuitAction::Succeed)),
                                (KeyModifiers::empty(), KeyCode::Char('o'), "Quit and print output", Action::Quit(QuitAction::PrintOutput)),
                                (KeyModifiers::empty(), KeyCode::Char('p'), "Quit and print pipeline", Action::Quit(QuitAction::PrintPipeline)),
                            ],
                            PrintOnExit::Pipeline => vec![
                                (KeyModifiers::empty(), KeyCode::Char('y'), "Print pipeline and quit", Action::Quit(QuitAction::PrintPipeline)),
                                (KeyModifiers::empty(), KeyCode::Char('n'), "Quit without printing", Action::Quit(QuitAction::Cancel)),
                            ],
                            PrintOnExit::Output => vec![
                                (KeyModifiers::empty(), KeyCode::Char('y'), "Print output and quit", Action::Quit(QuitAction::PrintOutput)),
                                (KeyModifiers::empty(), KeyCode::Char('n'), "Quit without printing", Action::Quit(QuitAction::Cancel)),
                            ],
                        };
                        self.popup = Some(KeysPopup::new(keys));
                        None
                    }
                    KeyEvent { code: KeyCode::Char('y'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        self.popup = Some(KeysPopup::new(vec![
                            (KeyModifiers::empty(), KeyCode::Char('c'), "Focused stage command", Action::CopyToClipboard(ui::action::CopySource::FocusedStageCommand)),
                            (KeyModifiers::empty(), KeyCode::Char('p'), "Whole pipeline command", Action::CopyToClipboard(ui::action::CopySource::WholePipeline)),
                            (KeyModifiers::empty(), KeyCode::Char('s'), "Shown stage output", Action::CopyToClipboard(ui::action::CopySource::ShownStageOutput)),
                            (KeyModifiers::empty(), KeyCode::Char('o'), "Final output", Action::CopyToClipboard(ui::action::CopySource::FinalStageOuput)),
                        ]));
                        None
                    }
                    KeyEvent { code: KeyCode::Char('p'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        Some(Action::NewStageAbove)
                    }
                    KeyEvent { code: KeyCode::Char('n'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        Some(Action::NewStageBelow)
                    }
                    KeyEvent { code: KeyCode::Char('d'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        Some(Action::DeleteStage)
                    }
                    KeyEvent { code: KeyCode::Char('x'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        Some(Action::ToggleStage)
                    }
                    KeyEvent { code: KeyCode::Char('c'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL|KeyModifiers::SHIFT, ..} => {
                        Some(Action::AbortPipeline)
                    }
                    KeyEvent { code: KeyCode::Up, kind: KeyEventKind::Press, modifiers: KeyModifiers::NONE, ..} => {
                        Some(Action::FocusPreviousStage)
                    }
                    KeyEvent { code: KeyCode::Down, kind: KeyEventKind::Press, modifiers: KeyModifiers::NONE, ..} => {
                        Some(Action::FocusNextStage)
                    }
                    KeyEvent { code: KeyCode::Enter, kind: KeyEventKind::Press, modifiers: KeyModifiers::NONE, ..} => {
                        Some(Action::Execute)
                    }
                    KeyEvent { code: KeyCode::Enter, kind: KeyEventKind::Press, modifiers: KeyModifiers::ALT, ..} => {
                        Some(Action::ExecuteAll)
                    }
                    KeyEvent { code: KeyCode::Char(' '), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        Some(Action::ShowStageOutput)
                    }
                    KeyEvent { code: KeyCode::Char('l'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        Some(Action::LaunchPager)
                    }
                    KeyEvent { code: KeyCode::Char('v'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        Some(Action::LaunchEditor)
                    }
                    KeyEvent { code: KeyCode::Char('r'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        Some(Action::RestartPipeline)
                    }
                    key => {
                        Some(Action::Input(key))
                    }
                }
            }
            _ => None
        }
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::Quit(action) => {
                self.should_quit = Some(match action {
                    QuitAction::Succeed => QuitResult::Success,
                    QuitAction::Cancel => QuitResult::Cancel,
                    QuitAction::PrintPipeline => {
                        let cmds = std::mem::take(&mut self.pipeline)
                            .into_iter()
                            .map(|mut stage| stage.input.value_and_reset())
                            .collect();
                        QuitResult::Pipeline(cmds)
                    }
                    QuitAction::PrintOutput => {
                        if let Some(stage) = self.execution.pipeline.last_mut() {
                            QuitResult::Output(stage.output.slice(..))
                        } else {
                            QuitResult::Cancel
                        }
                    }
                });
            }
            Action::NewStageAbove => {
                self.pipeline.insert(self.focused_stage, Stage::new(self.id_gen.gen_id()));
            }
            Action::NewStageBelow => {
                self.focused_stage += 1;
                self.pipeline.insert(self.focused_stage, Stage::new(self.id_gen.gen_id()));
            }
            Action::DeleteStage => {
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
            Action::ToggleStage => {
                let enabled = &mut self.pipeline[self.focused_stage].enabled;
                *enabled = !*enabled;
            }
            Action::AbortPipeline => {
                log::info!("hard-terminate executions");
                {
                    let this = &mut *self;
                    this.execution.interrupt(0);
                };
            }
            Action::FocusPreviousStage => {
                // Move focus to previous stage.
                if self.focused_stage != 0 {
                    self.focused_stage -= 1;
                }
            }
            Action::FocusNextStage => {
                // Move focus to next stage.
                if self.focused_stage < self.pipeline.len() - 1 {
                    self.focused_stage += 1;
                }
            }
            Action::Execute => {
                self.create_pending_execution(ExecutionMode::TillFocused);
                self.shown_stage_index = self.focused_stage;
            }
            Action::ExecuteAll => {
                self.create_pending_execution(ExecutionMode::All);
                self.shown_stage_index = self.focused_stage;
            }
            Action::ShowStageOutput => {
                self.shown_stage_index = self.focused_stage;
            }
            Action::LaunchPager => {
                let _ = self.launch_pager();
            }
            Action::LaunchEditor => {
                let command = self.pipeline[self.focused_stage].input.value().to_string();
                let _ = self.launch_editor(command);
            }
            Action::Input(key) => {
                self.pipeline[self.focused_stage].input.handle_event(&Event::Key(key));
            }
            Action::CopyToClipboard(source) => {
                let text: Option<Cow<str>> = match source {
                    CopySource::FocusedStageCommand => Some(self.pipeline[self.focused_stage].input.value().into()),
                    CopySource::WholePipeline => {
                        Some(self.pipeline.iter().map(|stage| stage.input.value()).join(" | ").into())
                    }
                    CopySource::ShownStageOutput => {
                        let shown_stage_id = self.pipeline[self.shown_stage_index].id;
                        if let Some(stage) = self.execution.get_stage(shown_stage_id) &&
                            let Ok(output) = stage.output.slice_str(..)
                        {
                            Some(output.into())
                        } else {
                            None
                        }
                    }
                    CopySource::FinalStageOuput => {
                        if let Some(stage) = self.execution.pipeline.last() &&
                            let Ok(output) = stage.output.slice_str(..)
                        {
                            Some(output.into())
                        } else {
                            None
                        }
                    }
                };
                if let Some(text) = text {
                    self.clipboard.set_text(text).unwrap();
                }
            }
            Action::RestartPipeline => {
                self.can_reuse_execution = false;
                self.create_pending_execution(ExecutionMode::All);
            }
        }
    }

    fn create_pending_execution(&mut self, mode: ExecutionMode) {
        // Disabled commands are attached to preceeding enabled
        // command and show it's output.
        // Leading disabled commands are completely ignored.

        let mut pending_execution = PendingExecution { pipeline: Vec::new() };

        // Determine range of commands to execute.
        let end_idx = match mode {
            ExecutionMode::All => self.pipeline.len(),
            ExecutionMode::TillFocused => {
                let mut idx = self.focused_stage + 1;
                // Attach following disabled stages, since we get output
                // for them "for free".
                while let Some(stage) = self.pipeline.get(idx) && !stage.enabled {
                    idx += 1;
                }
                idx
            }
        };

        for stage in &self.pipeline[..end_idx] {
            if stage.enabled {
                pending_execution.pipeline.push(PendingStage {
                    stage_ids: vec![stage.id],
                    command: stage.input.value().to_string()
                });
            } else if let Some(pending_stage) = pending_execution.pipeline.last_mut() {
                pending_stage.stage_ids.push(stage.id);
            }
        }

        // Calculate how may stages may be reused and interrupt all others.
        let stages_to_reuse = pending_execution.calculate_number_of_stages_to_reuse(&self.execution);
        self.execution.interrupt(stages_to_reuse);

        self.pending_execution = Some(pending_execution);
    }

    /// Start pending execution if current one is finished.
    fn execute_pending(&mut self) {
        let Some(pending_execution) = self.pending_execution.take() else { return };
        assert!(self.execution.interrupted_stages_are_finished());

        let last_execution = std::mem::take(&mut self.execution);

        let reusable_stages = if self.can_reuse_execution {
            last_execution.reuse()
        } else {
            self.can_reuse_execution = true;
            Vec::new()
        };

        self.execution = pending_execution.execute(&self.options.resolve_shell(), reusable_stages);
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
        if i < self.execution.pipeline.len() - 1 {
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
            stage.eoutput.push_slice(buf);
            return;
        }

        // Stderr is closed.
        stage.stderr = None;
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
fn render_stage(
    frame: &mut Frame, stage: &Stage, exec: Option<&StageExecution>, area: Rect,
    focused: bool, options: &Options
) -> Position {
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
    let command = if options.syntax_highlight {
        let highlighted = Line::from(highlight_command_to_spans(stage.input.value(), command_style).unwrap());
        Paragraph::new(highlighted)
    } else {
        Paragraph::new(Span::styled(stage.input.value(), command_style))
    };
    frame.render_widget(command.scroll((0, scroll as u16)), command_area);

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
) -> io::Result<QuitResult> {
    let mut term_event_reader = crossterm::event::EventStream::new();

    loop {
        let external_program_active = app.external_process.is_some();

        if !external_program_active {
            terminal.draw(|f| app.render(f))?;
        }

        if app.pending_execution.is_some() && app.execution.interrupted_stages_are_finished() {
            app.execute_pending();
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

        if let Some(result) = app.should_quit {
            return Ok(result);
        }
    }

    Ok(QuitResult::Success)
}

#[tokio::main(flavor="current_thread")]
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

    match res? {
        QuitResult::Pipeline(pipeline) => {
            for (i, stage) in pipeline.iter().enumerate() {
                if i != 0 {
                    print!(" | ");
                }
                print!("{stage}");
            }
            println!();
        }
        QuitResult::Output(output) => {
            std::io::stdout().write_all(output.as_bytes())?;
        }
        QuitResult::Cancel => return Err("cancelled".into()),
        QuitResult::Success => (),
    }

    Ok(())
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

fn render_bytes(f: &mut Frame, buf: &[u8], area: Rect, style: Style, cache: &mut OutputRenderCache) {
    // Create text, reusing vector allocations.
    let mut lines = std::mem::take(&mut cache.lines_cache).recycle();
    lines.extend(cache.spans_caches.drain(..).map(|v| Line { spans: v.recycle(), ..Default::default() }));

    let mut text = Text::from(lines).style(style);
    crate::ui::binary::render_binary(buf, area.as_size(), &mut text, &mut cache.invalid_line);

    f.render_widget(&text, area);

    // Save vector allocations for reuse.
    cache.spans_caches.extend(text.lines.iter_mut().map(|line| std::mem::take(&mut line.spans).recycle()));
    cache.lines_cache = text.lines.recycle();
}
