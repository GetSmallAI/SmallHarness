use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::io::Write;

const GRAY: &str = "\x1b[90m";
const RESET: &str = "\x1b[0m";

pub async fn bordered_read_line() -> Result<String> {
    tokio::task::spawn_blocking(read_bordered).await?
}

pub async fn plain_read_line(prompt: String) -> Result<String> {
    tokio::task::spawn_blocking(move || read_plain(&prompt)).await?
}

fn read_plain(prompt: &str) -> Result<String> {
    let mut out = std::io::stdout();
    write!(out, "{prompt}")?;
    out.flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let result = (|| -> Result<String> {
        let mut line = String::new();
        loop {
            if let Event::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) = crossterm::event::read()?
            {
                if kind == KeyEventKind::Release {
                    continue;
                }
                match code {
                    KeyCode::Enter => {
                        writeln!(out)?;
                        out.flush()?;
                        return Ok(line);
                    }
                    KeyCode::Backspace if line.pop().is_some() => {
                        write!(out, "\x08 \x08")?;
                        out.flush()?;
                    }
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        writeln!(out)?;
                        out.flush()?;
                        crossterm::terminal::disable_raw_mode().ok();
                        std::process::exit(0);
                    }
                    KeyCode::Char(c) => {
                        line.push(c);
                        write!(out, "{c}")?;
                        out.flush()?;
                    }
                    _ => {}
                }
            }
        }
    })();
    crossterm::terminal::disable_raw_mode()?;
    result
}

fn read_bordered() -> Result<String> {
    let (cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
    let width = (cols.max(1)) as usize;
    let bar = "─".repeat(width);
    let border = format!("{GRAY}{bar}{RESET}");
    let mut out = std::io::stdout();
    let mut line = String::new();

    write!(out, "\n{border}\n")?;
    writeln!(out, "› {line}")?;
    write!(out, "{border}\x1b[1A\r\x1b[{}G", 3 + line.chars().count())?;
    out.flush()?;

    crossterm::terminal::enable_raw_mode()?;
    let result = (|| -> Result<String> {
        loop {
            if let Event::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) = crossterm::event::read()?
            {
                if kind == KeyEventKind::Release {
                    continue;
                }
                match code {
                    KeyCode::Enter => {
                        if line.is_empty() {
                            write!(out, "\x1b[1A\x1b[2K\x1b[1A\x1b[2K\r")?;
                        } else {
                            write!(out, "\x1b[1B\x1b[2K\r")?;
                        }
                        out.flush()?;
                        return Ok(std::mem::take(&mut line));
                    }
                    KeyCode::Backspace => {
                        line.pop();
                        write!(out, "\r\x1b[2K› {line}")?;
                        out.flush()?;
                    }
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        writeln!(out, "{RESET}")?;
                        out.flush()?;
                        crossterm::terminal::disable_raw_mode().ok();
                        std::process::exit(0);
                    }
                    KeyCode::Char(c) => {
                        line.push(c);
                        write!(out, "\r\x1b[2K› {line}")?;
                        out.flush()?;
                    }
                    _ => {}
                }
            }
        }
    })();
    crossterm::terminal::disable_raw_mode()?;
    result
}
