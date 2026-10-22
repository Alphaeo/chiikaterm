mod agent;
mod app;
mod canvas;
mod channel;
mod config;
mod fx;
mod keys;
mod layout;
mod mascot;
mod session;
mod shell;
mod theme;
mod ui;

use std::fs::File;
use std::io::stdout;
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::Instant;

use anyhow::Context;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use ratatui::crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::crossterm::{execute, queue};
use ratatui::layout::Rect;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

use crate::app::{App, AppEvent, Paths};

/// `--help` text, built from the same table as the in-app bindings window.
fn usage() -> String {
    let mut text = String::from(
        "Usage: chiika [--version | --help]\nOpens a terminal workspace with several shells.\n",
    );
    for (section, keys) in ui::BINDINGS {
        text += &format!("\n{section}\n");
        for (keys, what) in *keys {
            text += &format!("  {keys:<18}{what}\n");
        }
    }
    text + "\nSessions are not persisted: quitting chiika kills every shell."
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [a] if a == "--version" || a == "-V" => {
            println!("chiika {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        // Run by the agents' Claude Code hooks: forward the hook to the chiika that started them.
        [a] if a == "hook" => {
            run_hook();
            return Ok(());
        }
        [a] if a == "--help" || a == "-h" => {
            println!("{}", usage());
            return Ok(());
        }
        _ => {
            eprintln!(
                "chiika: unexpected argument(s): {}\n\n{}",
                args.join(" "),
                usage()
            );
            std::process::exit(2);
        }
    }

    init_logging()?;
    tracing::info!("starting chiika {}", env!("CARGO_PKG_VERSION"));

    let mut terminal = ratatui::init();
    let ratatui_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
        ratatui_hook(info);
    }));
    // Windows: crossterm reads console key events, so a paste already arrives as typed keys and
    // enabling bracketed paste would only inject its markers as stray keys.
    let result = execute!(stdout(), EnableMouseCapture)
        .and_then(|()| {
            if cfg!(windows) {
                Ok(())
            } else {
                execute!(stdout(), EnableBracketedPaste)
            }
        })
        .map_err(anyhow::Error::from)
        .and_then(|()| run(&mut terminal));
    let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    result
}

/// Sends the hook JSON read on stdin to the chiika channel. Silent and always successful: an
/// agent must never be blocked or disturbed because chiika is gone.
fn run_hook() {
    use std::io::Read;
    let var = |name: &str| std::env::var(name).ok();
    let (Some(port), Some(token), Some(session)) = (
        var("CHIIKA_PORT").and_then(|p| p.parse().ok()),
        var("CHIIKA_TOKEN"),
        var("CHIIKA_SESSION"),
    ) else {
        return;
    };
    let mut input = Vec::new();
    if std::io::stdin()
        .take(256 * 1024)
        .read_to_end(&mut input)
        .is_ok()
    {
        let _ = channel::send(port, &token, &session, &input);
    }
}

/// Logs go to the file named by `CHIIKA_LOG` only: stdout belongs to the TUI.
fn init_logging() -> anyhow::Result<()> {
    let Some(path) = std::env::var_os("CHIIKA_LOG") else {
        return Ok(());
    };
    let file = File::create(&path).with_context(|| format!("creating log file {path:?}"))?;
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .with_env_var("CHIIKA_LOG_LEVEL")
        .from_env_lossy();
    tracing_subscriber::fmt()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .with_env_filter(filter)
        .init();
    Ok(())
}

fn run(terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel();
    let input_tx = tx.clone();
    thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            loop {
                match event::read() {
                    Ok(ev) => {
                        if input_tx.send(AppEvent::Input(ev)).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        tracing::error!("reading terminal input failed: {e}");
                        return;
                    }
                }
            }
        })
        .context("spawning input thread")?;

    let size = terminal.size()?;
    let paths = Paths {
        config: config::config_path(),
        project: std::env::current_dir().ok().map(|dir| dir.join(".chiika")),
        exe: std::env::current_exe().ok(),
    };
    let mut app = App::new(Rect::new(0, 0, size.width, size.height), tx, paths);
    while !app.should_quit {
        let now = Instant::now();
        app.tick(now);
        // Synchronized output: the terminal shows each frame at once, no tearing mid-animation.
        queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
        terminal.draw(|frame| ui::draw(frame, &app, now))?;
        execute!(terminal.backend_mut(), EndSynchronizedUpdate)?;
        if let Ok(ev) = rx.recv_timeout(app.frame_interval(Instant::now())) {
            app.handle(ev);
            // One frame per burst of events.
            while let Ok(ev) = rx.try_recv() {
                app.handle(ev);
            }
        }
    }
    app.kill_all();
    Ok(())
}
