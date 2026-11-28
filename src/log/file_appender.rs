// File appender that keeps all log output in a single size-limited file.
//
// When the file reaches `max_file_size`, the oldest complete entries are
// dropped (rolling cleanup) so the newest logs are retained. Unlike
// truncating the whole file, this keeps recent history available while
// still bounding disk usage to a single file.

use crate::error::{MhoError, Result};
use crate::log::LogLevel;
use crate::utils::fs::ensure_dir;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::PathBuf;

/// Chunk size used when shifting the retained tail of the log file.
const SHIFT_CHUNK: usize = 64 * 1024;

pub struct FileAppender {
    path: PathBuf,
    current_file: File,
    current_size: u64,
    max_file_size: u64,
}

impl FileAppender {
    pub fn new(log_dir: PathBuf, max_file_size: u64) -> Result<Self> {
        ensure_dir(&log_dir)?;

        let path = log_dir.join("mho.log");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .open(&path)
            .map_err(|e| {
                MhoError::Log(format!("Failed to open log file {}: {}", path.display(), e))
            })?;
        let current_size = file
            .metadata()
            .map_err(|e| {
                MhoError::Log(format!(
                    "Failed to read log file metadata {}: {}",
                    path.display(),
                    e
                ))
            })?
            .len();

        let mut appender = Self {
            path,
            current_file: file,
            current_size,
            max_file_size,
        };

        // A file left over from a previous run may exceed the current limit;
        // trim the oldest entries so it starts within bounds again.
        if appender.current_size > appender.max_file_size {
            appender.trim_to(appender.max_file_size)?;
        }
        appender.seek_to_end()?;
        Ok(appender)
    }

    fn seek_to_end(&mut self) -> Result<()> {
        self.current_file
            .seek(SeekFrom::End(0))
            .map_err(|e| MhoError::Log(format!("Failed to seek log file: {}", e)))?;
        Ok(())
    }

    fn clear(&mut self) -> Result<()> {
        self.current_file
            .set_len(0)
            .map_err(|e| MhoError::Log(format!("Failed to truncate log file: {}", e)))?;
        self.current_file
            .seek(SeekFrom::Start(0))
            .map_err(|e| MhoError::Log(format!("Failed to seek log file: {}", e)))?;
        self.current_size = 0;
        Ok(())
    }

    /// Shrink the log file to at most `keep` bytes by dropping the oldest
    /// complete entries (everything before the first newline after the cut
    /// point). The retained tail is shifted to the start of the file.
    fn trim_to(&mut self, keep: u64) -> Result<()> {
        if self.current_size <= keep {
            return Ok(());
        }
        self.current_file
            .flush()
            .map_err(|e| MhoError::Log(format!("Failed to flush log file: {}", e)))?;

        let mut reader = File::open(&self.path).map_err(|e| {
            MhoError::Log(format!(
                "Failed to open log file {} for reading: {}",
                self.path.display(),
                e
            ))
        })?;
        let mut buf = vec![0u8; SHIFT_CHUNK];

        // Find the first newline at or after the desired cut point so the
        // retained tail starts on a line boundary.
        let mut cut = self.current_size - keep;
        let start = loop {
            reader
                .seek(SeekFrom::Start(cut))
                .map_err(|e| MhoError::Log(format!("Failed to seek log file: {}", e)))?;
            let read = reader
                .read(&mut buf)
                .map_err(|e| MhoError::Log(format!("Failed to read log file: {}", e)))?;
            if read == 0 {
                break None;
            }
            if let Some(pos) = buf[..read].iter().position(|&b| b == b'\n') {
                break Some(cut + pos as u64 + 1);
            }
            cut += read as u64;
        };

        let Some(start) = start else {
            // No line boundary to keep (single oversized line): drop it all.
            return self.clear();
        };

        // Source offset is always greater than the destination offset, so a
        // forward chunked copy is safe.
        let mut read_pos = start;
        let mut write_pos = 0u64;
        while read_pos < self.current_size {
            let to_read = (self.current_size - read_pos).min(SHIFT_CHUNK as u64) as usize;
            reader
                .seek(SeekFrom::Start(read_pos))
                .map_err(|e| MhoError::Log(format!("Failed to seek log file: {}", e)))?;
            reader
                .read_exact(&mut buf[..to_read])
                .map_err(|e| MhoError::Log(format!("Failed to read log file: {}", e)))?;
            self.current_file
                .seek(SeekFrom::Start(write_pos))
                .map_err(|e| MhoError::Log(format!("Failed to seek log file: {}", e)))?;
            self.current_file
                .write_all(&buf[..to_read])
                .map_err(|e| MhoError::Log(format!("Failed to write to log file: {}", e)))?;
            read_pos += to_read as u64;
            write_pos += to_read as u64;
        }

        self.current_file
            .set_len(write_pos)
            .map_err(|e| MhoError::Log(format!("Failed to truncate log file: {}", e)))?;
        self.current_file
            .seek(SeekFrom::End(0))
            .map_err(|e| MhoError::Log(format!("Failed to seek log file: {}", e)))?;
        self.current_size = write_pos;
        Ok(())
    }

    pub fn write_log(&mut self, _level: LogLevel, message: &str) -> Result<()> {
        if self.max_file_size == 0 {
            return Ok(());
        }

        // A single entry must always fit, so cap it to leave room for the
        // trailing newline.
        let payload = Self::truncate_message(message, self.max_file_size - 1);
        let needed = payload.len() as u64 + 1;
        if self.current_size.saturating_add(needed) > self.max_file_size {
            self.trim_to(self.max_file_size - needed)?;
        }

        self.current_file
            .write_all(payload.as_bytes())
            .map_err(|e| MhoError::Log(format!("Failed to write to log file: {}", e)))?;
        self.current_file
            .write_all(b"\n")
            .map_err(|e| MhoError::Log(format!("Failed to write newline to log file: {}", e)))?;
        self.current_file
            .flush()
            .map_err(|e| MhoError::Log(format!("Failed to flush log file: {}", e)))?;
        self.current_size += needed;
        Ok(())
    }

    fn truncate_message(message: &str, max_bytes: u64) -> &str {
        if message.len() as u64 <= max_bytes {
            return message;
        }
        let mut end = max_bytes as usize;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        &message[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("mho-log-test-{}-{}", std::process::id(), id));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn log(&self) -> String {
            std::fs::read_to_string(self.0.join("mho.log")).unwrap()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn keeps_newest_entries_and_only_one_file() {
        let dir = TestDir::new();
        let mut appender = FileAppender::new(dir.0.clone(), 12).unwrap();

        appender.write_log(LogLevel::Info, "1111").unwrap();
        appender.write_log(LogLevel::Info, "2222").unwrap();
        appender.write_log(LogLevel::Info, "3333").unwrap();

        assert_eq!(dir.log(), "2222\n3333\n");
        assert!(std::fs::metadata(dir.0.join("mho.log")).unwrap().len() <= 12);
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn startup_trims_oversized_file() {
        let dir = TestDir::new();
        std::fs::write(dir.0.join("mho.log"), "old1\nold2\nold3\nold4\n").unwrap();

        let mut appender = FileAppender::new(dir.0.clone(), 10).unwrap();
        appender.write_log(LogLevel::Info, "new").unwrap();

        assert_eq!(dir.log(), "old4\nnew\n");
        assert!(std::fs::metadata(dir.0.join("mho.log")).unwrap().len() <= 10);
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn trims_across_multiple_chunks_and_keeps_complete_lines() {
        let dir = TestDir::new();
        let max = SHIFT_CHUNK as u64 + 1000;
        let mut appender = FileAppender::new(dir.0.clone(), max).unwrap();

        for i in 0..200 {
            appender
                .write_log(LogLevel::Info, &format!("{:04}-{}", i, "x".repeat(1000)))
                .unwrap();
        }

        let contents = dir.log();
        assert!(contents.len() as u64 <= max);
        assert!(contents.ends_with(&format!("0199-{}\n", "x".repeat(1000))));
        assert!(contents.lines().all(|line| line.len() == 1005));
    }

    #[test]
    fn oversized_entry_is_truncated_within_utf8_boundary() {
        let dir = TestDir::new();
        let mut appender = FileAppender::new(dir.0.clone(), 8).unwrap();

        appender.write_log(LogLevel::Info, "超长日志消息").unwrap();

        let contents = std::fs::read(dir.0.join("mho.log")).unwrap();
        assert!(contents.len() <= 8);
        assert!(std::str::from_utf8(&contents).is_ok());
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }
}
