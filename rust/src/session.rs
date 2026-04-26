use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::openai::ChatMessage;

#[derive(Serialize, Deserialize)]
struct SessionEntry {
    timestamp: String,
    message: ChatMessage,
}

pub fn init_session_dir(dir: &str) -> Result<()> {
    let p = Path::new(dir);
    if !p.exists() {
        std::fs::create_dir_all(p)?;
    }
    Ok(())
}

pub fn new_session_path(dir: &str) -> PathBuf {
    let id = chrono::Utc::now()
        .format("%Y-%m-%dT%H-%M-%S-%3f")
        .to_string()
        + "Z";
    Path::new(dir).join(format!("{id}.jsonl"))
}

pub fn save_message(path: &Path, message: &ChatMessage) -> Result<()> {
    let entry = SessionEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        message: message.clone(),
    };
    let line = serde_json::to_string(&entry)?;
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}
