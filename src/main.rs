use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
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
use tui_input::{backend::crossterm::EventHandler, Input};

#[derive(Debug)]
enum AppEvent {
    Input(Event),
    CommandOutput(String),
    CommandError(String),
}

struct App {
    input: Input,
    output_lines: Vec<String>,
    should_quit: bool,
    current_command: String,
}

impl App {
    fn new() -> App {
        App {
            input: Input::default(),
            output_lines: vec!["Enter a command and press Enter to execute it.".to_string()],
            should_quit: false,
            current_command: String::new(),
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
                            self.current_command = self.input.value().to_string();
                            self.output_lines.clear();
                            self.output_lines.push(format!("$ {}", self.input.value()));
                            self.output_lines.push("Executing...".to_string());
                            self.input.reset();
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

    // Input area - use tui-input's built-in rendering
    let input = Paragraph::new(app.input.value())
        .block(Block::default().borders(Borders::ALL).title("Command (Ctrl-Q to quit)"));
    f.render_widget(input, chunks[0]);
    
    // Set cursor position
    f.set_cursor_position((
        chunks[0].x + app.input.visual_cursor() as u16 + 1,
        chunks[0].y + 1,
    ));

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
                AppEvent::Input(input_event) => {
                    app.handle_input(input_event);
                }
                AppEvent::CommandOutput(output) => {
                    app.handle_command_output(output);
                }
                AppEvent::CommandError(error) => {
                    app.handle_command_error(error);
                }
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
