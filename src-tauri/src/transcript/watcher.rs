//! JSONL transcript file watcher & stream emitter (WP-02).
//!
//! Tracks live transcript `.jsonl` files, seeks to last read position, parses new
//! records, and emits structured events over `transcript://{session_id}` bus.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::time::sleep;

use super::parser::{parse_line, TranscriptRecord};

/// Reads newly appended records from `file_path` starting from `offset`.
/// Updates `offset` to the new end-of-file position.
pub fn read_new_records(file_path: &Path, offset: &mut u64) -> Vec<TranscriptRecord> {
    let mut records = Vec::new();
    let file = match File::open(file_path) {
        Ok(f) => f,
        Err(_) => return records,
    };

    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(*offset)).is_err() {
        return records;
    }

    let mut line = String::new();
    while let Ok(bytes_read) = reader.read_line(&mut line) {
        if bytes_read == 0 {
            break;
        }
        if let Some(record) = parse_line(&line) {
            records.push(record);
        }
        line.clear();
    }

    if let Ok(new_pos) = reader.stream_position() {
        *offset = new_pos;
    }

    records
}

/// Spawns a background task that watches `transcript_path` for changes
/// and emits parsed records over `app.emit(&format!("transcript://{}", session_id), &records)`.
/// Returns an `Arc<AtomicBool>` stop signal handle.
pub fn watch_transcript_session(
    app: AppHandle,
    session_id: String,
    transcript_path: PathBuf,
) -> Arc<AtomicBool> {
    let stop_signal = Arc::new(AtomicBool::new(false));
    let stop_flag = stop_signal.clone();
    let event_channel = format!("transcript://{}", session_id);

    tokio::spawn(async move {
        let mut offset = 0u64;

        // Perform initial catch-up read of all existing turns
        let initial_records = read_new_records(&transcript_path, &mut offset);
        if !initial_records.is_empty() {
            let _ = app.emit(&event_channel, &initial_records);
        }

        // Tail watcher loop (~250ms polling interval)
        while !stop_flag.load(Ordering::Relaxed) {
            sleep(Duration::from_millis(250)).await;

            if transcript_path.exists() {
                let new_records = read_new_records(&transcript_path, &mut offset);
                if !new_records.is_empty() {
                    let _ = app.emit(&event_channel, &new_records);
                }
            }
        }
    });

    stop_signal
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_read_new_records_incremental() {
        let mut tmp = NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();

        writeln!(
            tmp,
            "{}",
            r#"{"type":"user","uuid":"1","message":{"role":"user","content":[{"type":"text","text":"line 1"}]}}"#
        )
        .expect("write 1");

        let mut offset = 0u64;
        let records1 = read_new_records(&path, &mut offset);
        assert_eq!(records1.len(), 1);

        // Incremental append
        writeln!(
            tmp,
            "{}",
            r#"{"type":"summary","uuid":"2","summaryText":"compaction done"}"#
        )
        .expect("write 2");

        let records2 = read_new_records(&path, &mut offset);
        assert_eq!(records2.len(), 1);
        match &records2[0] {
            TranscriptRecord::Summary { summary_text, .. } => {
                assert_eq!(summary_text.as_deref(), Some("compaction done"));
            }
            _ => panic!("expected Summary"),
        }
    }
}
