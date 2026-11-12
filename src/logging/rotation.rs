//! Size rotation preserves whole JSON records, with two retained archives.
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

pub struct RotatingFile {
    directory: PathBuf,
    file: File,
    bytes: u64,
    max_bytes: u64,
}

impl RotatingFile {
    pub fn new(directory: &Path, max_bytes: u64) -> io::Result<Self> {
        std::fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let file = open_private(&directory.join("proxy.jsonl"))?;
        let bytes = file.metadata()?.len();
        Ok(Self {
            directory: directory.into(),
            file,
            bytes,
            max_bytes,
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let oldest = self.directory.join("proxy.2.jsonl");
        match std::fs::remove_file(&oldest) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let archive = self.directory.join("proxy.1.jsonl");
        match std::fs::rename(&archive, oldest) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let active = self.directory.join("proxy.jsonl");
        std::fs::rename(&active, archive)?;
        self.file = open_private(&active)?;
        self.bytes = 0;
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, record: &[u8]) -> io::Result<usize> {
        // tracing formats each event before calling write_all; rotate between events.
        if self.bytes > 0 && self.bytes.saturating_add(record.len() as u64) > self.max_bytes {
            self.rotate()?;
        }
        self.file.write_all(record)?;
        self.bytes = self.bytes.saturating_add(record.len() as u64);
        Ok(record.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn open_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}
