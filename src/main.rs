//! Stevedore: a keyboard-driven TUI for managing Docker containers.
//! Architecture: Model-View-Update.
//!
//! - Model/Update: `app::App` and `app::Message` (src/app.rs)
//! - View: `ui::draw` (src/ui.rs)
//! - Side effects: a background Docker actor (src/docker.rs) that receives
//!   `DockerCommand`s and reports back with `Message`s over channels.

mod app;
mod docker;
mod ui;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use app::{App, Message};

#[tokio::main]
async fn main() -> Result<()> {
    // ratatui::init installs a panic hook that restores the terminal,
    // enables raw mode, and enters the alternate screen.
    let terminal = ratatui::init();
    let result = run(terminal).await;
    ratatui::restore();
    result
}

async fn run(mut terminal: ratatui::DefaultTerminal) -> Result<()> {
    // Messages flowing into the update loop (from Docker tasks and input).
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Message>();
    // Commands flowing out to the Docker actor.
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<docker::DockerCommand>();

    docker::spawn(msg_tx, cmd_rx);

    let mut app = App::new(cmd_tx);
    let mut input = EventStream::new();

    loop {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        // Wait for either a terminal input event or a message from the
        // Docker background tasks, whichever arrives first. Neither branch
        // blocks the other, so the UI stays responsive while polling.
        tokio::select! {
            maybe_event = input.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        app.update(Message::Key(key));
                    }
                    // Resize and other events just trigger a redraw.
                    Some(Ok(_)) => {}
                    Some(Err(err)) => {
                        app.update(Message::DockerError(format!("Input error: {err}")));
                    }
                    None => break,
                }
            }
            maybe_msg = msg_rx.recv() => {
                match maybe_msg {
                    Some(msg) => app.update(msg),
                    None => break,
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}
