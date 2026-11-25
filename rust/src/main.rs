//! Terminal Studio Code: a VS Code look-alike that runs entirely inside a terminal.
//!
//! `tscode [path]` - path is a folder (workspace) or a file to open.

mod app;
mod completion;
mod editor;
mod highlight;
mod icons;
mod languages;
mod popups;
mod screen;
mod settings;
mod sidebar;
mod term;
mod theme;
mod theme_data;
mod ui;
mod util;

use app::{App, Ev};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

const HELP: &str = "\
usage: tscode [-h] [--theme THEME] [--list-themes] [path]

Terminal Studio Code - VS Code in your terminal.

positional arguments:
  path           folder to open, or a file (default: current folder)

options:
  -h, --help     show this help message and exit
  --theme THEME  colour theme id for this run (see --list-themes)
  --list-themes  print the available theme ids and exit
  --version      print the version and exit";

fn fail(message: &str) -> ! {
    eprintln!("tscode: {message}");
    std::process::exit(1);
}

/// Put the terminal back the way we found it.
fn restore_terminal(enhanced: bool) {
    let mut out = io::stdout();
    if enhanced {
        let _ = queue!(out, PopKeyboardEnhancementFlags);
    }
    // Give the terminal its own cursor shape back.
    // ...and its normal mouse pointer.
    let _ = out.write_all(b"\x1b[0 q\x1b[?25h\x1b]22;default\x1b\\");
    let _ = execute!(out, DisableBracketedPaste, DisableMouseCapture, LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
}

fn main() {
    let mut path: Option<String> = None;
    let mut theme: Option<String> = None;
    let mut list_themes = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return;
            }
            "--version" => {
                println!("tscode {} (Rust)", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--list-themes" => list_themes = true,
            "--theme" => theme = Some(args.next().unwrap_or_else(|| fail("--theme needs a theme id"))),
            other if other.starts_with("--theme=") => theme = Some(other["--theme=".len()..].to_string()),
            other if other.starts_with('-') && other.len() > 1 => fail(&format!("unknown option '{other}'. Try `tscode --help`.")),
            other if path.is_none() => path = Some(other.to_string()),
            other => fail(&format!("unexpected argument '{other}'. Try `tscode --help`.")),
        }
    }
    let themes = theme::available_themes();
    if list_themes {
        for (id, label) in themes {
            println!("{id:24} {label}");
        }
        return;
    }
    if let Some(id) = &theme {
        if themes.iter().all(|(known, _)| known != id) {
            fail(&format!("unknown theme '{id}'. Run `tscode --list-themes` to see the choices."));
        }
    }

    let given = path.unwrap_or_else(|| ".".to_string());
    let typed = match given.strip_prefix("~/") {
        Some(rest) => settings::home().join(rest),
        None => PathBuf::from(&given),
    };
    let target = std::fs::canonicalize(&typed).unwrap_or_else(|_| fail(&format!("no such file or directory: {given}")));
    let (workspace, files) = if target.is_dir() {
        (target, Vec::new())
    } else {
        (target.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("/")), vec![target])
    };

    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        fail("this is a full-screen app; run it in a terminal.");
    }
    if let Err(error) = run(workspace, files, theme) {
        fail(&error.to_string());
    }
}

fn run(workspace: PathBuf, files: Vec<PathBuf>, theme: Option<String>) -> io::Result<()> {
    terminal::enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    // Ctrl+Shift+letter and Ctrl+` only arrive in terminals with the kitty keyboard protocol.
    let enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
    }
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal(enhanced);
        hook(info);
    }));

    let (tx, rx) = channel::<Ev>();
    let input = tx.clone();
    std::thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            if input.send(Ev::Input(event)).is_err() {
                break;
            }
        }
    });

    let (w, h) = terminal::size()?;
    let mut app = App::new(workspace, files, theme, tx, (w as i32, h as i32));
    let mut renderer = screen::Renderer::default();
    let mut size = app.size;
    loop {
        if app.size != size {
            size = app.size;
            renderer.reset();
        }
        if app.dirty {
            app.dirty = false;
            let (buffer, cursor) = app.draw();
            let mut frame = std::mem::take(&mut app.out);
            if cursor.is_some() {
                frame.push_str("\x1b[6 q"); // a thin bar, like VS Code's
            }
            frame.push_str(&renderer.render(&buffer, cursor));
            out.write_all(frame.as_bytes())?;
            out.flush()?;
        }
        if app.quit {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                app.handle(event);
                // Take what else is waiting, but never so long that the screen goes stale.
                let started = Instant::now();
                while started.elapsed() < Duration::from_millis(12) {
                    match rx.try_recv() {
                        Ok(event) => app.handle(event),
                        Err(_) => break,
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        app.tick();
    }
    drop(app); // stops the shell and Claude
    restore_terminal(enhanced);
    Ok(())
}
