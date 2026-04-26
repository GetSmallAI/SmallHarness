use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::config::LoaderStyle;

const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

const SPINNER_FRAMES: &[&str] = &[
    "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏",
];
const GRADIENT_COLORS: &[&str] = &[
    "\x1b[38;5;240m",
    "\x1b[38;5;245m",
    "\x1b[38;5;250m",
    "\x1b[38;5;255m",
    "\x1b[38;5;250m",
    "\x1b[38;5;245m",
];

pub struct Loader {
    stop: Arc<AtomicBool>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Loader {
    pub fn start(text: String, style: LoaderStyle) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_inner = stop.clone();
        let interval_ms: u64 = match style {
            LoaderStyle::Gradient => 150,
            LoaderStyle::Spinner => 80,
            LoaderStyle::Minimal => 300,
        };
        let handle = tokio::spawn(async move {
            let mut frame: usize = 0;
            draw(frame, &text, style);
            loop {
                tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                if stop_inner.load(Ordering::Relaxed) {
                    break;
                }
                frame = frame.wrapping_add(1);
                draw(frame, &text, style);
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let mut out = std::io::stdout();
        let _ = write!(out, "\r\x1b[K");
        let _ = out.flush();
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

fn draw(frame: usize, text: &str, style: LoaderStyle) {
    let mut out = std::io::stdout();
    match style {
        LoaderStyle::Minimal => {
            let dots = ["·", "··", "···"];
            let _ = write!(out, "\r{DIM}{text}{}{RESET}", dots[frame % 3]);
        }
        LoaderStyle::Spinner => {
            let ch = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
            let _ = write!(out, "\r{DIM}{ch} {text}{RESET}");
        }
        LoaderStyle::Gradient => {
            let len = GRADIENT_COLORS.len();
            let mut s = String::from("\r");
            for (i, ch) in text.chars().enumerate() {
                s.push_str(GRADIENT_COLORS[(frame + i) % len]);
                s.push(ch);
            }
            s.push_str(RESET);
            let _ = out.write_all(s.as_bytes());
        }
    }
    let _ = out.flush();
}
