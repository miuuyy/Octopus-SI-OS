use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::Path;

#[cfg(target_vendor = "apple")]
mod compression;

pub mod publication;
pub mod metadata;

pub fn sync_ordered(file: &File) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        file.sync_all()
    }
}

pub fn sync_filesystem_ordered(file: &File) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::syncfs(file.as_raw_fd()) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(libc::ENOSYS | libc::EPERM | libc::EOPNOTSUPP | libc::EINVAL)
        ) {
            return Ok(false);
        }
        Err(error)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Ok(false)
    }
}

pub fn sync_directory_ordered(path: &Path) -> io::Result<()> {
    sync_ordered(&File::open(path)?)
}

pub fn write_new(path: &Path, bytes: &[u8]) -> io::Result<File> {
    #[cfg(target_vendor = "apple")]
    let prepared = compression::FileCompression::open(
        path.parent()
            .ok_or_else(|| io::Error::other("new file has no parent"))?,
    )?
    .map(|compression| compression.prepare(path, bytes))
    .transpose()?
    .flatten();
    #[cfg(not(target_vendor = "apple"))]
    let prepared: Option<File> = None;
    let mut file = match prepared {
        Some(file) => file,
        None => {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path)?;
            if let Err(error) = file.write_all(bytes) {
                drop(file);
                let _ = std::fs::remove_file(path);
                return Err(error);
            }
            file
        }
    };
    let verified = (|| {
        file.rewind()?;
        let mut buffer = [0u8; 65536];
        for chunk in bytes.chunks(buffer.len()) {
            file.read_exact(&mut buffer[..chunk.len()])?;
            if buffer[..chunk.len()] != *chunk {
                return Err(io::Error::other("new file failed exact byte verification"));
            }
        }
        if file.read(&mut buffer[..1])? != 0 {
            return Err(io::Error::other("new file contains additional bytes"));
        }
        Ok(())
    })();
    if let Err(error) = verified {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(file)
}
