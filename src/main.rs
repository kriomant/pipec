#![feature(result_option_map_or_default)]

use append_only_bytes::{AppendOnlyBytes, BytesSlice};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{stream::FuturesUnordered, FutureExt as _, StreamExt as _};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Flex, Layout, Position, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame, Terminal,
};
use std::{
    io::{self}, os::unix::process::ExitStatusExt, process::{ExitStatus, Stdio}
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _}, process::{Child, ChildStderr, ChildStdin, ChildStdout, Command}, select,
};
use tui_input::{backend::crossterm::EventHandler, Input};

struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,

    // Number of bytes written to stdin from previous stage's output.
    bytes_written_to_stdin: usize,
}

enum ExecutionState {
    Running(Process),
    Finished(ExitStatus),
}

struct Execution {
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

struct App {
    input: Input,
    should_quit: bool,
    show_help: bool,

    pipeline: Vec<Stage>,

    // Index of focused pipe in `pipeline`.
    // This is command currently edited by user.
    focused_stage: usize,
}

impl App {
    fn new() -> App {
        App {
            input: Input::default(),
            should_quit: false,
            show_help: true,
            pipeline: vec![
                Stage {
                    command: String::new(),
                    execution: None,
                    pending_command: None,
                },
            ],
            focused_stage: 0,
        }
    }

    fn handle_input(&mut self, event: Event) {
        match event {
            Event::Key(key) => {
                match key {
                    KeyEvent { code: KeyCode::Char('q'), kind: KeyEventKind::Press, modifiers: KeyModifiers::CONTROL, ..} => {
                        self.should_quit = true;
                    }
                    KeyEvent { code: KeyCode::Enter, kind: KeyEventKind::Press, modifiers: KeyModifiers::NONE, ..} => {
                        if !self.input.value().trim().is_empty() {
                            self.show_help = false;
                            let stage = &mut self.pipeline[0];
                            if let Some(execution) = &mut stage.execution &&
                               let ExecutionState::Running(process) = &mut execution.state {
                                // We can't immediately start new program instance because old one
                                // is still running. Remember current command and terminate old
                                // one.
                                stage.pending_command = Some(self.input.value().to_string());
                                process.child.start_kill().unwrap();
                            } else {
                                stage.execution = Some(start_command(self.input.value(), self.focused_stage != 0).unwrap());
                            }
                        }
                    }
                    _ => {
                        self.input.handle_event(&event);
                        self.pipeline[0].command = self.input.value().to_string();
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_process_terminated(&mut self, i: usize, status: ExitStatus) -> std::io::Result<()> {
        let stage = &mut self.pipeline[i];
        let execution = stage.execution.as_mut().unwrap();
        execution.state = ExecutionState::Finished(status);

        // Even though process is terminated, there may be some data in stdout/stderr
        // buffers, so don't close them immediately.
        // However, if there is pending command, then we are not interested in complete
        // result of current one anyway, so just close buffers and reset output.
        if let Some(command) = stage.pending_command.take() {
            stage.execution = Some(start_command(&command, i != 0)?);
        } else {
            match status.code() {
                Some(code) => execution.output.push_str(&format!("\nProcess exited with code {}", code)),
                None => execution.output.push_str(&format!("\nProcess exited with code {}", status.signal().unwrap())),
            }
        }
        Ok(())
    }

    fn handle_stdin(&mut self, i: usize, bytes_written: usize) {
        if bytes_written == 0 {
            let Some(Execution { state: ExecutionState::Running(p), .. }) = &mut self.pipeline[i].execution else {
                panic!("unexpected state");
            };
            p.stdin = None;
            return;
        }

        let Some(Execution { state: ExecutionState::Running(p), .. }) = &mut self.pipeline[i-1].execution else {
            panic!("unexpected state");
        };
        p.bytes_written_to_stdin += bytes_written;
    }

    fn handle_stdout(&mut self, i: usize, buf: &[u8]) {
        if buf.is_empty() {
            let Some(Execution { state: ExecutionState::Running(p), .. }) = &mut self.pipeline[i].execution else {
                panic!("unexpected state");
            };
            p.stdout = None;
            return;
        }

        let stage = &mut self.pipeline[i];
        let execution = stage.execution.as_mut().unwrap();
        execution.output.push_slice(buf);
    }

    fn handle_stderr(&mut self, i: usize, buf: &[u8]) {
        if buf.is_empty() {
            let Some(Execution { state: ExecutionState::Running(p), .. }) = &mut self.pipeline[i].execution else {
                panic!("unexpected state");
            };
            p.stderr = None;
            return;
        }

        let stage = &mut self.pipeline[i];
        let execution = stage.execution.as_mut().unwrap();
        execution.output.push_slice(buf);
    }
}

fn ui(f: &mut Frame, app: &App) {
    // Each stage takes one line.
    let mut constraints: Vec<_> = app.pipeline.iter().map(|_| Constraint::Length(1)).collect();
    // Output pane takes the rest.
    constraints.push(Constraint::Min(0));

    let stage_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(f.area());
    let (output_area, stage_areas) = stage_areas.split_last().unwrap();

    for (i, (stage, area)) in app.pipeline.iter().zip(stage_areas.iter()).enumerate() {
        let command_pos = render_stage(f, stage, *area);

        if app.focused_stage == i {
            f.set_cursor_position((
                command_pos.x + app.input.visual_cursor() as u16,
                command_pos.y,
            ));
        }
    }

    // Output area
    let output = str::from_utf8(app.pipeline.last().unwrap().execution.as_ref().map_or_default(|e| e.output.as_bytes())).unwrap();
    let output_widget = Paragraph::new(output);
    f.render_widget(output_widget, *output_area);

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
                Span::styled("Enter  ", key_style),
                Span::raw("Execute command"),
            ]),
            Line::from(vec![
                Span::styled("Ctrl+Q ", key_style),
                Span::raw("Exit program"),
            ]),
            Line::from(vec![
                Span::styled("↑/↓    ", key_style),
                Span::raw("Go to previous/next stage"),
            ]),
        ]);

        let [introduction_area, keys_area] =
            Layout::vertical([
                Constraint::Length(introduction_text.lines.len() as u16),
                Constraint::Length(keys_help.lines.len() as u16),
            ])
            .flex(Flex::Center)
            .areas(*output_area);
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
fn render_stage(frame: &mut Frame, stage: &Stage, area: Rect) -> Position {
    let [marker_area, command_area, status_area] = Layout
        ::horizontal([
            Constraint::Min(1),
            Constraint::Percentage(100),
            Constraint::Min(1),
        ])
        .spacing(1)
        .areas(area);

    // Input area with green prompt sign
    frame.render_widget(Span::styled("❯", Style::default().fg(Color::Green)), marker_area);

    // Command
    frame.render_widget(Span::raw(&stage.command), command_area);

    // Status indicator
    let status_span = match &stage.execution {
        None => Span::raw(" "),  // Command is running
        Some(ex) => match ex.state {
            ExecutionState::Running(_) => Span::styled("•", Style::default().fg(Color::Yellow)),  // Command is running
            ExecutionState::Finished(status) => {
                if status.success() {
                    Span::styled("✔︎", Style::default().fg(Color::Green))  // Success
                } else {
                    Span::styled("✖︎", Style::default().fg(Color::Red))  // Failed
                }
            }
        }
    };
    let status = Paragraph::new(Line::from(vec![status_span]))
        .alignment(Alignment::Right);
    frame.render_widget(status, status_area);

    return command_area.as_position();
}

fn start_command(command: &str, stdin: bool) -> std::io::Result<Execution> {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", &command]);
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &command]);
        cmd
    };

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
        state: ExecutionState::Running(Process { child, stdin, stdout, stderr, bytes_written_to_stdin: 0 }),
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
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut app: App,
) -> io::Result<()> {
    let mut term_event_reader = crossterm::event::EventStream::new();

    loop {
        terminal.draw(|f| ui(f, &app))?;

        let event = {
            let mut exit_futures = FuturesUnordered::new();
            let mut stdin_futures = FuturesUnordered::new();
            let mut stdout_futures = FuturesUnordered::new();
            let mut stderr_futures = FuturesUnordered::new();

            app.pipeline.iter_mut().enumerate().fold(None, |prev_stdout: Option<BytesSlice>, (i, stage)| {
                let Some(Execution {state: ExecutionState::Running(p), output, ..}) = &mut stage.execution else { return None };

                exit_futures.push(p.child.wait().map(move |status| (i, status)));
                if let (Some(stdin), Some(prev_stdout)) = (&mut p.stdin, prev_stdout) {
                    let bytes_written_to_stdin = p.bytes_written_to_stdin;
                    stdin_futures.push(async move {
                        let buf = &prev_stdout.as_bytes()[bytes_written_to_stdin..];
                        let res = stdin.write(buf).await;
                        (i, res)
                    });
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

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    // Panic handler to reset terminalAdd commentMore actions
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        disable_raw_mode().unwrap();
        execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture
        ).unwrap();

        prev_hook(info);
    }));

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app and run it
    let app = App::new();
    let res = run_app(&mut terminal, app).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err);
    }

    Ok(())
}
