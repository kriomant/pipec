use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{future::OptionFuture, StreamExt as _};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Flex, Layout},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame, Terminal,
};
use std::{
    fmt::Write as _, io, process::{ExitStatus, Stdio}, os::unix::process::ExitStatusExt
};
use tokio::{
    io::AsyncReadExt as _, process::{Child, ChildStderr, ChildStdout, Command}, select,
};
use tui_input::{backend::crossterm::EventHandler, Input};

struct Process {
    child: Child,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
}

struct App {
    input: Input,
    should_quit: bool,
    show_help: bool,

    // Content of input at the moment Enter was pressed.
    // If old child is still alive at this point, command in saved
    // as pending awaiting until old child exits.
    pending_command: Option<String>,

    current_process: Option<Process>,
    command_output: String,
    exit_status: Option<ExitStatus>,
}

impl App {
    fn new() -> App {
        App {
            input: Input::default(),
            should_quit: false,
            show_help: true,
            pending_command: None,
            current_process: None,
            command_output: String::new(),
            exit_status: None,
        }
    }

    fn handle_input(&mut self, event: Event) {
        match event {
            Event::Key(key) => {
                match key.code {
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.should_quit = true;
                    }
                    KeyCode::Enter => {
                        if !self.input.value().trim().is_empty() {
                            self.show_help = false;
                            if let Some(process) = &mut self.current_process {
                                // We can't immediately start new program instance because old one
                                // is still running. Remember current command and terminate old
                                // one.
                                self.pending_command = Some(self.input.value().to_string());
                                process.child.start_kill().unwrap();
                            } else {
                                self.command_output.clear();
                                self.exit_status = None;
                                self.current_process = Some(start_command(self.input.value()).unwrap());
                            }
                        }
                    }
                    _ => {
                        self.input.handle_event(&event);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_process_terminated(&mut self, status: ExitStatus) {
        self.exit_status = Some(status);
        
        match status.code() {
            Some(code) => writeln!(self.command_output, "\nProcess exited with code {}", code).unwrap(),
            None => writeln!(self.command_output, "\nProcess exited with code {}", status.signal().unwrap()).unwrap(),
        }
    }

    fn handle_stdout(&mut self, buf: &[u8]) {
        let text = std::str::from_utf8(buf).unwrap();
        self.command_output.push_str(text);
    }
    fn handle_stderr(&mut self, buf: &[u8]) {
        let text = std::str::from_utf8(buf).unwrap();
        self.command_output.push_str(text);
    }
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)].as_ref())
        .split(f.area());

    // Status indicator
    let status_span = match &app.exit_status {
        None if app.current_process.is_none() => Span::raw(" "),  // Command is running
        None => Span::styled("•", Style::default().fg(Color::Yellow)),  // Command is running
        Some(status) => {
            if status.success() {
                Span::styled("✔︎", Style::default().fg(Color::Green))  // Success
            } else {
                Span::styled("✖︎", Style::default().fg(Color::Red))  // Failed
            }
        }
    };

    // Input area with green prompt sign and status indicator
    let input_line = Line::from(vec![
        Span::styled("❯ ", Style::default().fg(Color::Green)),
        Span::raw(app.input.value()),
    ]);
    let input = Paragraph::new(input_line);
    f.render_widget(input, chunks[0]);

    // Status indicator aligned to the right
    let status = Paragraph::new(Line::from(vec![status_span]))
        .alignment(Alignment::Right);
    f.render_widget(status, chunks[0]);

    // Set cursor position (accounting for the green prompt sign)
    f.set_cursor_position((
        chunks[0].x + app.input.visual_cursor() as u16 + 2, // +2 for "❯ " prefix
        chunks[0].y,
    ));

    // Output area
    let output_area = chunks[1];
    let output = Paragraph::new(app.command_output.as_str());
    f.render_widget(output, output_area);

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

fn start_command(command: &str) -> std::io::Result<Process> {
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
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    assert!(stdout.is_some() && stderr.is_some());
    Ok(Process { child, stdout, stderr })
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut app: App,
) -> io::Result<()> {
    let mut term_event_reader = crossterm::event::EventStream::new();

    let mut stdout_buf = [0u8; 1024];
    let mut stderr_buf = [0u8; 1024];

    loop {
        terminal.draw(|f| ui(f, &app))?;

        let (child, stdout, stderr) = if let Some(p) = app.current_process.as_mut() {
            (Some(&mut p.child), p.stdout.as_mut(), p.stderr.as_mut())
        } else {
            (None, None, None)
        };

        select! {
            result = term_event_reader.next() => {
                match result {
                    Some(Ok(event)) => app.handle_input(event),
                    _ => break
                }
            }
            Some(status) = OptionFuture::from(child.map(|c| c.wait())) => {
                app.handle_process_terminated(status?);
                app.current_process = None;

                // Even though process is terminated, there may be some data in stdout/stderr
                // buffers, so don't close them immediately.
                // However, if there is pending command, then we are not interested in complete
                // result of current one anyway, so just close buffers and reset output.
                if let Some(command) = app.pending_command.take() {
                    app.command_output.clear();
                    app.exit_status = None;
                    app.current_process = Some(start_command(&command)?);
                }
            }
            Some(bytes_read) = OptionFuture::from(stdout.map(|s| s.read(&mut stdout_buf))) => {
                let bytes_read = bytes_read?;
                if bytes_read != 0 {
                    app.handle_stdout(&stdout_buf[..bytes_read]);
                } else {
                    app.current_process.as_mut().unwrap().stdout = None;
                }
            }
            Some(bytes_read) = OptionFuture::from(stderr.map(|s| s.read(&mut stderr_buf))) => {
                let bytes_read = bytes_read?;
                if bytes_read != 0 {
                    app.handle_stderr(&stderr_buf[..bytes_read]);
                } else {
                    app.current_process.as_mut().unwrap().stderr = None;
                }
            }
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
