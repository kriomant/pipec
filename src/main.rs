use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::{
    io,
    process::Stdio,
    time::Duration,
};
use tokio::{
    process::Command,
    sync::mpsc,
    time::timeout,
};

#[derive(Debug)]
enum AppEvent {
    Input(Event),
    CommandOutput(String),
    CommandError(String),
}

struct App {
    input: String,
    output_lines: Vec<String>,
    cursor_position: usize,
    should_quit: bool,
    current_command: String,
}

impl App {
    fn new() -> App {
        App {
            input: String::new(),
            output_lines: vec!["Enter a command and press Enter to execute it.".to_string()],
            cursor_position: 0,
            should_quit: false,
            current_command: String::new(),
        }
    }

    fn handle_input(&mut self, key_code: KeyCode, modifiers: KeyModifiers) {
        match key_code {
            KeyCode::Char('q') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Enter => {
                if !self.input.trim().is_empty() {
                    self.current_command = self.input.clone();
                    self.output_lines.clear();
                    self.output_lines.push(format!("$ {}", self.input));
                    self.output_lines.push("Executing...".to_string());
                }
            }
            KeyCode::Char(c) => {
                self.input.insert(self.cursor_position, c);
                self.cursor_position += 1;
            }
            KeyCode::Backspace => {
                if self.cursor_position > 0 {
                    self.input.remove(self.cursor_position - 1);
                    self.cursor_position -= 1;
                }
            }
            KeyCode::Delete => {
                if self.cursor_position < self.input.len() {
                    self.input.remove(self.cursor_position);
                }
            }
            KeyCode::Left => {
                if self.cursor_position > 0 {
                    self.cursor_position -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor_position < self.input.len() {
                    self.cursor_position += 1;
                }
            }
            KeyCode::Home => {
                self.cursor_position = 0;
            }
            KeyCode::End => {
                self.cursor_position = self.input.len();
            }
            _ => {}
        }
    }

    fn handle_command_output(&mut self, output: String) {
        self.output_lines.clear();
        self.output_lines.push(format!("$ {}", self.current_command));
        
        if output.trim().is_empty() {
            self.output_lines.push("(no output)".to_string());
        } else {
            for line in output.lines() {
                self.output_lines.push(line.to_string());
            }
        }
    }

    fn handle_command_error(&mut self, error: String) {
        self.output_lines.clear();
        self.output_lines.push(format!("$ {}", self.current_command));
        self.output_lines.push(format!("Error: {}", error));
    }
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)].as_ref())
        .split(f.area());

    // Input area
    let input_text = if app.cursor_position < app.input.len() {
        vec![
            Span::raw(&app.input[..app.cursor_position]),
            Span::styled(
                &app.input[app.cursor_position..app.cursor_position + 1],
                Style::default().bg(Color::White).fg(Color::Black),
            ),
            Span::raw(&app.input[app.cursor_position + 1..]),
        ]
    } else {
        vec![
            Span::raw(&app.input),
            Span::styled(" ", Style::default().bg(Color::White)),
        ]
    };

    let input = Paragraph::new(Line::from(input_text))
        .block(Block::default().borders(Borders::ALL).title("Command (Ctrl-Q to quit)"));
    f.render_widget(input, chunks[0]);

    // Output area
    let output_items: Vec<ListItem> = app
        .output_lines
        .iter()
        .map(|line| ListItem::new(line.as_str()))
        .collect();

    let output = List::new(output_items)
        .block(Block::default().borders(Borders::ALL).title("Output"));
    f.render_widget(output, chunks[1]);
}

async fn execute_command(command: String) -> Result<String, String> {
    let timeout_duration = Duration::from_secs(30);
    
    let result = timeout(timeout_duration, async {
        let mut cmd = if cfg!(target_os = "windows") {
            let mut cmd = Command::new("cmd");
            cmd.args(["/C", &command]);
            cmd
        } else {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", &command]);
            cmd
        };

        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
    }).await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            
            if !stderr.is_empty() {
                Ok(format!("{}\nstderr: {}", stdout, stderr))
            } else {
                Ok(stdout.to_string())
            }
        }
        Ok(Err(e)) => Err(format!("Failed to execute command: {}", e)),
        Err(_) => Err("Command timed out after 30 seconds".to_string()),
    }
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut app: App,
) -> io::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    
    // Spawn input handler
    let input_tx = tx.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(event) = event::read() {
                if input_tx.send(AppEvent::Input(event)).is_err() {
                    break;
                }
            }
        }
    });

    let mut current_command = String::new();

    loop {
        terminal.draw(|f| ui(f, &app))?;

        // Check if we need to execute a new command
        if app.current_command != current_command && !app.current_command.is_empty() {
            current_command = app.current_command.clone();
            let command = current_command.clone();
            let cmd_tx = tx.clone();
            
            tokio::spawn(async move {
                match execute_command(command).await {
                    Ok(output) => {
                        let _ = cmd_tx.send(AppEvent::CommandOutput(output));
                    }
                    Err(error) => {
                        let _ = cmd_tx.send(AppEvent::CommandError(error));
                    }
                }
            });
        }

        // Handle events
        if let Ok(event) = rx.try_recv() {
            match event {
                AppEvent::Input(Event::Key(key)) => {
                    app.handle_input(key.code, key.modifiers);
                }
                AppEvent::CommandOutput(output) => {
                    app.handle_command_output(output);
                }
                AppEvent::CommandError(error) => {
                    app.handle_command_error(error);
                }
                _ => {}
            }
        }

        if app.should_quit {
            break;
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
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
