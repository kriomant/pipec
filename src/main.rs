#![feature(result_option_map_or_default)]

use append_only_bytes::{AppendOnlyBytes, BytesSlice};
use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{stream::FuturesUnordered, FutureExt as _, StreamExt as _};
use itertools::Itertools as _;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Flex, Layout, Position, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame, Terminal,
};
use std::{
    fs::File, io::{self}, os::unix::process::ExitStatusExt, process::{ExitStatus, Stdio}
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _}, process::{Child, ChildStderr, ChildStdin, ChildStdout, Command}, select,
};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::options::Options;

mod parser;

struct Process {
    child: Child,

    status: Option<ExitStatus>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,

    // Number of bytes written to stdin from previous stage's output.
    bytes_written_to_stdin: usize,
}
impl Process {
    fn finished(&self) -> bool {
        self.status.is_some() && self.stdout.is_none() && self.stderr.is_none()
    }
}

mod options;

enum ExecutionState {
    Running(Process),
    Finished(ExitStatus),
}

struct Execution {
    command: String,
    state: ExecutionState,
    output: AppendOnlyBytes,
}

struct Stage {
    // Current command entered by user.
    // It may not be the same command with wich `execution` is started.
    // And it's not necessary command which will be executed next.
    command: String,

    // Process being executed or finished, together with it's output.
    execution: Option<Execution>,

    // Content of input at the moment Enter was pressed.
    // If old child is still alive at this point, command in saved
    // as pending awaiting until old child exits.
    pending_command: Option<String>,
}

impl Stage {
    fn new() -> Self {
        Self {
            command: String::new(),
            execution: None,
            pending_command: None,
        }
    }

    fn with_command(command: String) -> Self {
        Self {
            command,
            execution: None,
            pending_command: None,
        }
    }

    /// Returns whether the current command differs from the command used in the last execution.
    fn command_changed(&self) -> bool {
        self.execution.as_ref().map(|e| e.command != self.command).unwrap_or(true)
    }

    /// Returns whether stage needs (re)execution.
    fn needs_execution(&self) -> bool {
        // Stage needs execution if …

        // it wasn't executed at all
        let Some(ex) = &self.execution else { return true };

        // or execution was unsuccessful
        let success = match &ex.state {
            ExecutionState::Running(process) => process.status.map_or(true, |s| s.success()),
            ExecutionState::Finished(exit_status) => exit_status.success(),
        };
        if !success {
            return true;
        }

        // or command changed since last run
        if ex.command != self.command {
            return true;
        }

        false
    }
}

struct App {
    options: Options,

    input: Input,
    should_quit: bool,
    show_help: bool,

    pipeline: Vec<Stage>,

    // Index of focused pipe in `pipeline`.
    // This is command currently edited by user.
    focused_stage: usize,

    // Index of shown pipe.
    // This is pipe whose output is shown to user.
    shown_stage: usize,
}

impl App {
    fn new(mut options: Options) -> Result<App, Box<dyn std::error::Error>> {
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

        let mut pipeline: Vec<_> = commands.into_iter().map(Stage::with_command).collect();
        if pipeline.is_empty() {
            pipeline.push(Stage::new());
        }
        let focused_stage = pipeline.len() - 1;
        let input = Input::new(pipeline[focused_stage].command.clone());
        let shown_stage = focused_stage;

        Ok(App {
            options,
            input,
            should_quit: false,
            show_help: true,
            pipeline,
            focused_stage,
            shown_stage,
        })
    }

    fn handle_input(&mut self, event: Event) {
        match event {
            Event::Key(key) => {
                match key {
                    KeyEvent { code: KeyCode::Char('q'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        self.should_quit = true;
                    }
                    KeyEvent { code: KeyCode::Char('p'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        self.pipeline.insert(self.focused_stage, Stage::new());
                        self.input.reset();
                    }
                    KeyEvent { code: KeyCode::Char('n'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        self.focused_stage += 1;
                        self.pipeline.insert(self.focused_stage, Stage::new());
                        self.input.reset();
                    }
                    KeyEvent { code: KeyCode::Char('d'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        if self.pipeline.len() > 1 {
                            self.pipeline.remove(self.focused_stage);
                            if self.focused_stage != 0 {
                                self.focused_stage -= 1;
                            }
                            self.input = Input::new(self.pipeline[self.focused_stage].command.clone());

                            if self.shown_stage >= self.pipeline.len() {
                                self.shown_stage = self.pipeline.len() - 1;
                            }
                        }
                    }
                    KeyEvent { code: KeyCode::Char('c'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL|KeyModifiers::SHIFT, ..} => {
                        log::info!("hard-terminate executions");
                        for stage in &mut self.pipeline {
                            if let Some(Execution { state: ExecutionState::Running(Process { child, ..}), .. }) = &mut stage.execution {
                                child.start_kill().unwrap();
                            }
                        }
                    }
                    KeyEvent { code: KeyCode::Up, kind: KeyEventKind::Press, modifiers: KeyModifiers::NONE, ..} => {
                        // Move focus to previous stage.
                        if self.focused_stage != 0 {
                            self.focused_stage -= 1;
                            self.input = Input::new(self.pipeline[self.focused_stage].command.clone());
                        }
                    }
                    KeyEvent { code: KeyCode::Down, kind: KeyEventKind::Press, modifiers: KeyModifiers::NONE, ..} => {
                        // Move focus to next stage.
                        if self.focused_stage < self.pipeline.len() - 1 {
                            self.focused_stage += 1;
                            self.input = Input::new(self.pipeline[self.focused_stage].command.clone());
                        }
                    }
                    KeyEvent { code: KeyCode::Enter, kind: KeyEventKind::Press, modifiers: KeyModifiers::NONE, ..} => {
                        self.show_help = false;

                        let stages = self.pipeline.iter_mut()
                            .enumerate()
                            // Skip starting stages which are still actual: they have been already executed
                            // (successfully) in the past and command didn't change since last execution.
                            .skip_while(|(_, s)| !s.needs_execution());

                        for (i, stage) in stages {
                            if let Some(execution) = &mut stage.execution &&
                                let ExecutionState::Running(process) = &mut execution.state
                            {
                                // We can't immediately start new program instance because old one
                                // is still running. Remember current command and terminate old
                                // one.
                                stage.pending_command = Some(stage.command.clone());
                                process.child.start_kill().unwrap();
                            } else {
                                stage.execution = Some(start_command(stage.command.clone(), i != 0).unwrap());
                            }
                        }
                    }
                    KeyEvent { code: KeyCode::Char(' '), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        self.shown_stage = self.focused_stage;
                    }
                    _ => {
                        self.input.handle_event(&event);
                        self.pipeline[self.focused_stage].command = self.input.value().to_string();
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_process_terminated(&mut self, i: usize, status: ExitStatus) -> std::io::Result<()> {
        log::info!("stage {}: teminated: {:?}", i, status);
        let stage = &mut self.pipeline[i];

        // Stage must be active.
        let Some(Execution { state: ExecutionState::Running(p), .. }) = stage.execution.as_mut() else {
            panic!("invalid state");
        };

        // If there is pending command, then we are not interested in complete
        // result of current execution, so just close buffers and start new
        // execution.
        if let Some(command) = stage.pending_command.take() {
            stage.execution = Some(start_command(command, i != 0)?);
            return Ok(());
        }

        // Otherwise, even though process is terminated, there may be some data left
        // in stdout/stderr buffers, so don't switch execution to 'finished' state
        // until they are read out and closed.
        assert!(p.status.is_none());
        p.status = Some(status);
        if !p.finished() {
            return Ok(());
        }

        log::info!("stage {}: finished", i);
        stage.execution.as_mut().unwrap().state = ExecutionState::Finished(status);

        Ok(())
    }

    fn handle_stdin(&mut self, i: usize, bytes_written: usize) {
        log::info!("stage {}: written {} bytes to stdin", i, bytes_written);
        let Some(Execution { state: ExecutionState::Running(p), .. }) = &mut self.pipeline[i].execution else {
            panic!("unexpected state");
        };

        if bytes_written == 0 {
            p.stdin = None;
            return;
        }

        p.bytes_written_to_stdin += bytes_written;
        log::debug!("stage {}: {} bytes written to stdin", i, p.bytes_written_to_stdin);

        let total_written = p.bytes_written_to_stdin;
        let should_close_stdin = {
            let prev_exec = &mut self.pipeline[i-1].execution.as_ref().unwrap();
            total_written ==
                prev_exec.output.len() &&
                matches!(prev_exec.state,
                         ExecutionState::Finished(_) | ExecutionState::Running(Process {stdout: None, ..}))
        };

        if should_close_stdin {
            let Some(Execution { state: ExecutionState::Running(p), .. }) = &mut self.pipeline[i].execution else {
                panic!("unexpected state");
            };
            p.stdin = None;
        }
    }

    fn handle_stdout(&mut self, i: usize, buf: &[u8]) {
        log::info!("stage {}: read {} bytes from stdout", i, buf.len());
        let stage = &mut self.pipeline[i];

        let exit_status = {
            // Stage must be active.
            let Some(Execution { state: ExecutionState::Running(p), output, .. }) = stage.execution.as_mut() else {
                panic!("invalid state");
            };

            if !buf.is_empty() {
                output.push_slice(buf);
                return;
            }

            // Stdout is closed.
            p.stdout = None;
            let finished = p.finished();

            if finished {
                log::info!("stage {}: finished", i);
                p.status
            } else {
                None
            }
        };

        if let Some(exit_status) = exit_status {
            let new_state = ExecutionState::Finished(exit_status);
            stage.execution.as_mut().unwrap().state = new_state;
        }

        // Close stdin of next stage, if all data are already written.
        let output_len = stage.execution.as_mut().unwrap().output.len();
        if i < self.pipeline.len() - 1 {
            if let Some(Execution { state: ExecutionState::Running(p), .. }) = self.pipeline[i+1].execution.as_mut() {
                if p.bytes_written_to_stdin == output_len {
                    p.stdin = None;
                }
            }
        }
    }

    fn handle_stderr(&mut self, i: usize, buf: &[u8]) {
        log::info!("stage {}: read {} bytes from stderr", i, buf.len());
        let stage = &mut self.pipeline[i];

        let exit_status = {
            // Stage must be active.
            let Some(Execution { state: ExecutionState::Running(p), output, .. }) = stage.execution.as_mut() else {
                panic!("invalid state");
            };

            if !buf.is_empty() {
                output.push_slice(buf);
                return;
            }

            // Stderr is closed.
            p.stderr = None;
            let finished = p.finished();

            if finished {
                log::info!("stage {}: finished", i);
                p.status
            } else {
                None
            }
        };

        if let Some(exit_status) = exit_status {
            let new_state = ExecutionState::Finished(exit_status);
            stage.execution.as_mut().unwrap().state = new_state;
        }

        // Close stdin of next stage, if all data are already written.
        let output_len = stage.execution.as_mut().unwrap().output.len();
        if i < self.pipeline.len() - 1 {
            if let Some(Execution { state: ExecutionState::Running(p), .. }) = self.pipeline[i+1].execution.as_mut() {
                if p.bytes_written_to_stdin == output_len {
                    p.stdin = None;
                }
            }
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    // Output of shown stage is displayed right before it. So shown stage and
    // stages before it are shown above output area and others are shown below.

    // Each stage takes one line.
    let mut constraints: Vec<_> = std::iter::repeat(Constraint::Length(1))
        .take(app.pipeline.len())
        .collect();
    // Output pane takes the rest.
    constraints.insert(app.shown_stage+1, Constraint::Min(0));

    let mut stage_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(f.area())
        .to_vec();
    let output_area = stage_areas.remove(app.shown_stage+1);

    for (i, (stage, area)) in app.pipeline.iter().zip(stage_areas.iter()).enumerate() {
        let command_pos = render_stage(f, stage, *area, i == app.focused_stage);

        if app.focused_stage == i {
            f.set_cursor_position((
                command_pos.x + app.input.visual_cursor() as u16,
                command_pos.y,
            ));
        }
    }

    // Output area
    let output = str::from_utf8(app.pipeline[app.shown_stage].execution.as_ref().map_or_default(|e| e.output.as_bytes())).unwrap();
    let output_widget = Paragraph::new(output);
    f.render_widget(output_widget, output_area);

    if app.show_help {
        let key_style = Style::default().fg(Color::Yellow);

        let introduction_text = Text::from(vec![
            Line::from(""),
            Line::from(Span::styled("Welcome to Plumber!", Style::default().fg(Color::Cyan))),
            Line::from(""),
            Line::from(vec![Span::raw("Type a command and press "), Span::styled("Enter", key_style), Span::raw(" to get started.")]),
            Line::from(""),
        ]);
        let keys_help = Text::from(vec![
            Line::from(vec![
                Span::styled("Enter        ", key_style),
                Span::raw("Execute command"),
            ]),
            Line::from(vec![
                Span::styled("Ctrl+Space   ", key_style),
                Span::raw("Show output of current stage"),
            ]),
            Line::from(vec![
                Span::styled("Ctrl+Q       ", key_style),
                Span::raw("Exit program"),
            ]),
            Line::from(vec![
                Span::styled("Ctrl-N       ", key_style),
                Span::raw("Add new stage below"),
            ]),
            Line::from(vec![
                Span::styled("Ctrl-P       ", key_style),
                Span::raw("Add new stage above"),
            ]),
            Line::from(vec![
                Span::styled("Ctrl-D       ", key_style),
                Span::raw("Delete focused stage"),
            ]),
            Line::from(vec![
                Span::styled("Ctrl-Shift-C ", key_style),
                Span::raw("Hard-stop execution (send KILL)"),
            ]),
            Line::from(vec![
                Span::styled("↑/↓          ", key_style),
                Span::raw("Go to previous/next stage"),
            ]),
        ]);

        let [introduction_area, keys_area] =
            Layout::vertical([
                Constraint::Length(introduction_text.lines.len() as u16),
                Constraint::Length(keys_help.lines.len() as u16),
            ])
            .flex(Flex::Center)
            .areas(output_area);
        let [introduction_area] =
            Layout::horizontal([Constraint::Length(introduction_text.width() as u16)])
            .flex(Flex::Center)
            .areas(introduction_area);
        let [keys_area] =
            Layout::horizontal([Constraint::Length(keys_help.width() as u16)])
            .flex(Flex::Center)
            .areas(keys_area);

        f.render_widget(
            Paragraph::new(introduction_text) .alignment(Alignment::Center),
            introduction_area
        );
        f.render_widget(
            Paragraph::new(keys_help),
            keys_area
        );
    }
}

/// Renders stage into given area.
/// Returns position of command widget.
fn render_stage(frame: &mut Frame, stage: &Stage, area: Rect, focused: bool) -> Position {
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
    if stage.command_changed() {
        command_style = command_style.bold()
    }
    frame.render_widget(Span::styled(&stage.command, command_style), command_area);

    // Status indicator
    let status_span = match &stage.execution {
        None => Span::raw(" "),  // Command is running
        Some(ex) => match ex.state {
            ExecutionState::Running(_) => Span::styled("•", Style::default().fg(Color::Yellow)),  // Command is running
            ExecutionState::Finished(status) => {
                if status.success() {
                    Span::styled("✔︎", Style::default().fg(Color::Green))  // Success
                } else if status.code().is_some() {
                    Span::styled("✖︎", Style::default().fg(Color::Red))  // Failed
                } else if status.signal().is_some() {
                    Span::styled("ѳ", Style::default().fg(Color::Red))  // Killed by signal
                } else {
                    Span::styled("?", Style::default().fg(Color::Red))  // Unknown
                }
            }
        }
    };
    let status = Paragraph::new(Line::from(vec![status_span]))
        .alignment(Alignment::Right);
    frame.render_widget(status, status_area);

    return command_area.as_position();
}

fn start_command(command: String, stdin: bool) -> std::io::Result<Execution> {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", &command]);
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &command]);
        cmd
    };

    log::info!("start command: {:?}", cmd);
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

    Ok(Execution {
        command,
        state: ExecutionState::Running(Process {
            child,
            status: None,
            stdin,
            stdout,
            stderr,
            bytes_written_to_stdin: 0,
        }),
        output: AppendOnlyBytes::new(),
    })
}

enum UiEvent {
    Term(crossterm::event::Event),
    Stage(usize, StageEvent),
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
        terminal.draw(|f| ui(f, &app))?;

        let event = {
            let mut exit_futures = FuturesUnordered::new();
            let mut stdin_futures = FuturesUnordered::new();
            let mut stdout_futures = FuturesUnordered::new();
            let mut stderr_futures = FuturesUnordered::new();

            app.pipeline.iter_mut().enumerate().fold(None, |prev_stdout: Option<BytesSlice>, (i, stage)| {
                let Some(Execution {state, output, ..}) = &mut stage.execution else { return None };

                if let ExecutionState::Running(p) = state {
                    if p.status.is_none() {
                        exit_futures.push(p.child.wait().map(move |status| (i, status)));
                    }

                    if let (Some(stdin), Some(prev_stdout)) = (&mut p.stdin, prev_stdout) {
                        let bytes_written_to_stdin = p.bytes_written_to_stdin;
                        if bytes_written_to_stdin < prev_stdout.len() {
                            log::debug!("stage {}: {} of {} previous stage output bytes are written to stdin", i, p.bytes_written_to_stdin, prev_stdout.len());
                            stdin_futures.push(async move {
                                log::debug!("stage {}: schedule writing {} bytes", i, &prev_stdout.as_bytes()[bytes_written_to_stdin..].len());
                                let buf = &prev_stdout.as_bytes()[bytes_written_to_stdin..];
                                let res = stdin.write(buf).await;
                                (i, res)
                            });
                        }
                    }
                    if let Some(stdout) = &mut p.stdout {
                        stdout_futures.push(async move {
                            let mut buf = vec![0; 1024];
                            let res = stdout.read(&mut buf).await;
                            (i, res, buf)
                        });
                    }
                    if let Some(stderr) = &mut p.stderr {
                        stderr_futures.push(async move {
                            let mut buf = vec![0; 1024];
                            let res = stderr.read(&mut buf).await;
                            (i, res, buf)
                        });
                    }
                }

                Some(output.clone().to_slice())
            });

            select! {
                result = term_event_reader.next() => {
                    match result {
                        Some(Ok(event)) => UiEvent::Term(event),
                        _ => break
                    }
                }
                Some((i, status)) = exit_futures.next() => {
                    UiEvent::Stage(i, StageEvent::Exit(status?))
                }
                Some((i, res)) = stdin_futures.next() => {
                    UiEvent::Stage(i, StageEvent::Stdin(res?))
                }
                Some((i, res, buf)) = stdout_futures.next() => {
                    let bytes_read = res?;
                    UiEvent::Stage(i, StageEvent::Stdout(buf, bytes_read))
                }
                Some((i, res, buf)) = stderr_futures.next() => {
                    let bytes_read = res?;
                    UiEvent::Stage(i, StageEvent::Stderr(buf, bytes_read))
                }
            }
        };

        match event {
            UiEvent::Term(event) => app.handle_input(event),
            UiEvent::Stage(i, StageEvent::Exit(status)) => app.handle_process_terminated(i, status).unwrap(),
            UiEvent::Stage(i, StageEvent::Stdin(n)) => app.handle_stdin(i, n),
            UiEvent::Stage(i, StageEvent::Stdout(buf, n)) => app.handle_stdout(i, &buf[..n]),
            UiEvent::Stage(i, StageEvent::Stderr(buf, n)) => app.handle_stderr(i, &buf[..n]),
        }

        if app.should_quit {
            break;
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
            .parse_filters(&logging)
            .target(env_logger::Target::Pipe(Box::new(log_writer)))
            .try_init()?;
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut writer = io::stderr();
    execute!(writer, EnterAlternateScreen, EnableMouseCapture)?;

    // Panic handler to reset terminalAdd commentMore actions
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
            if app.options.print_command {
                for (i, stage) in app.pipeline.iter().enumerate() {
                    if i != 0 {
                        print!(" | ");
                    }
                    print!("{}", stage.command);
                }
                println!();
            }
        }
        Err(err) => {
            eprintln!("{:?}", err);
        }
    }

    Ok(())
}
