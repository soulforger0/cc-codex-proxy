use crate::{
    config::{AppPaths, LogConfig},
    error::{ProxyError, Result},
};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::Path,
};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub fn init(paths: &AppPaths, config: &LogConfig) -> Result<Vec<WorkerGuard>> {
    if config.max_bytes == 0 {
        return Err(ProxyError::Config(
            "log.max_bytes must be greater than zero".into(),
        ));
    }
    paths.ensure()?;
    let filter = if config.verbose {
        EnvFilter::new("cc_codex_proxy=debug,proxy_core=debug,tower_http=info")
    } else {
        EnvFilter::new("cc_codex_proxy=info,proxy_core=info,tower_http=warn")
    };

    let file_appender = CappedLogWriter::open(paths.logs_dir.join("proxy.log"), config.max_bytes)?;
    let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);
    let file_layer = fmt::layer()
        .json()
        .with_writer(file_writer)
        .with_ansi(false);

    let registry = tracing_subscriber::registry().with(filter).with(file_layer);
    if config.stderr {
        registry
            .with(fmt::layer().with_writer(std::io::stderr))
            .try_init()
            .ok();
    } else {
        registry.try_init().ok();
    }
    Ok(vec![file_guard])
}

#[derive(Debug)]
struct CappedLogWriter {
    file: File,
    current_len: u64,
    max_bytes: u64,
}

impl CappedLogWriter {
    fn open(path: impl AsRef<Path>, max_bytes: u64) -> io::Result<Self> {
        if max_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log max bytes must be greater than zero",
            ));
        }
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)?;
        let len = file.metadata()?.len();
        let current_len = if len > max_bytes {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            0
        } else {
            len
        };
        Ok(Self {
            file,
            current_len,
            max_bytes,
        })
    }

    fn ensure_capacity(&mut self, incoming_len: usize) -> io::Result<()> {
        let incoming_len = u64::try_from(incoming_len).unwrap_or(u64::MAX);
        if self.current_len.saturating_add(incoming_len) > self.max_bytes {
            self.file.set_len(0)?;
            self.file.seek(SeekFrom::Start(0))?;
            self.current_len = 0;
        }
        Ok(())
    }

    fn record_written(&mut self, written: usize) {
        self.current_len = self
            .current_len
            .saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
    }
}

impl Write for CappedLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.ensure_capacity(buf.len())?;
        let written = self.file.write(buf)?;
        self.record_written(written);
        Ok(written)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.ensure_capacity(buf.len())?;
        self.file.write_all(buf)?;
        self.record_written(buf.len());
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

pub fn redact_header(name: &str, value: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "authorization" | "cookie" | "set-cookie" | "chatgpt-account-id" | "x-cc-codex-admin-token"
    ) {
        format!("[redacted len={}]", value.len())
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn capped_writer_keeps_existing_file_under_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxy.log");
        fs::write(&path, b"existing\n").unwrap();

        let writer = CappedLogWriter::open(&path, 1024).unwrap();

        assert_eq!(writer.current_len, 9);
        drop(writer);
        assert_eq!(fs::read(&path).unwrap(), b"existing\n");
        assert_only_proxy_log(dir.path());
    }

    #[test]
    fn capped_writer_truncates_existing_file_over_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxy.log");
        fs::write(&path, b"too large").unwrap();

        let writer = CappedLogWriter::open(&path, 4).unwrap();

        assert_eq!(writer.current_len, 0);
        drop(writer);
        assert_eq!(fs::read(&path).unwrap(), b"");
        assert_only_proxy_log(dir.path());
    }

    #[test]
    fn capped_writer_truncates_and_continues_in_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxy.log");
        let mut writer = CappedLogWriter::open(&path, 10).unwrap();

        writer.write_all(b"12345").unwrap();
        writer.write_all(b"67890").unwrap();
        writer.write_all(b"tail").unwrap();
        writer.flush().unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"tail");
        assert_eq!(writer.current_len, 4);
        assert_only_proxy_log(dir.path());
    }

    #[test]
    fn capped_writer_writes_single_oversized_record_after_truncating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxy.log");
        let mut writer = CappedLogWriter::open(&path, 5).unwrap();

        writer.write_all(b"old").unwrap();
        writer.write_all(b"oversized").unwrap();
        writer.flush().unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"oversized");
        assert_eq!(writer.current_len, 9);
        assert_only_proxy_log(dir.path());
    }

    #[test]
    fn capped_writer_truncates_again_after_oversized_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxy.log");
        let mut writer = CappedLogWriter::open(&path, 5).unwrap();

        writer.write_all(b"oversized").unwrap();
        writer.write_all(b"ok").unwrap();
        writer.flush().unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"ok");
        assert_eq!(writer.current_len, 2);
        assert_only_proxy_log(dir.path());
    }

    fn assert_only_proxy_log(dir: &Path) {
        let mut entries = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(entries, vec!["proxy.log"]);
    }
}
