use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrollbackRecord {
    pub ts_ms: Option<i64>,
    pub dir: String, // "in" | "out"
    pub who: Option<String>,
    pub body: String,
}

pub fn append(scrollback_dir: &Path, conversation_key: &str, rec: &ScrollbackRecord) -> Result<()> {
    fs::create_dir_all(scrollback_dir)
        .with_context(|| format!("create scrollback dir {scrollback_dir:?}"))?;
    let path = path_for(scrollback_dir, conversation_key);
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open scrollback {path:?}"))?;
    let line = serde_json::to_string(rec).context("serialize scrollback record")?;
    f.write_all(line.as_bytes())
        .and_then(|_| f.write_all(b"\n"))
        .with_context(|| format!("append scrollback {path:?}"))?;
    Ok(())
}

pub fn load_tail(
    scrollback_dir: &Path,
    conversation_key: &str,
    limit: usize,
) -> Result<Vec<ScrollbackRecord>> {
    let path = path_for(scrollback_dir, conversation_key);
    if !path.exists() {
        return Ok(vec![]);
    }
    let f = OpenOptions::new()
        .read(true)
        .open(&path)
        .with_context(|| format!("open scrollback {path:?}"))?;
    let r = BufReader::new(f);
    let mut buf = Vec::new();
    for line in r.lines() {
        let line = line.context("read scrollback line")?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<ScrollbackRecord>(line) {
            Ok(v) => buf.push(v),
            Err(_) => {
                // Ignore corrupted/older lines.
            }
        }
    }
    if buf.len() > limit {
        buf.drain(0..(buf.len() - limit));
    }
    Ok(buf)
}

fn path_for(scrollback_dir: &Path, conversation_key: &str) -> PathBuf {
    let hex = hex_encode(conversation_key.as_bytes());
    scrollback_dir.join(format!("{hex}.jsonl"))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}
