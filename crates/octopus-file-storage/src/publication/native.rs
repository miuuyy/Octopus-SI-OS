use super::*;
use std::ffi::{OsStr, OsString};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NodeState {
    pub(super) identity: FileIdentity,
    pub(super) kind: DirectoryEntryKind,
    pub(super) mode: u32,
    pub(super) bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
fn unsupported() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "directory publication requires native descriptor-relative traversal")
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn name(value: &OsStr) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(value.as_encoded_bytes()).map_err(|_| invalid("filesystem entry contains NUL"))
}

pub(super) fn child(parent: &File, value: &OsStr, directory: bool) -> io::Result<File> {
    #[cfg(any(target_vendor = "apple", target_os = "linux"))] {
        use std::os::fd::{AsRawFd, FromRawFd};
        let name = name(value)?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK
            | if directory { libc::O_DIRECTORY } else { 0 };
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if if directory { !metadata.is_dir() } else { !metadata.is_file() } {
            return Err(invalid("descriptor-relative entry has a different filesystem type"));
        }
        Ok(file)
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))] {
        let _ = (parent, value, directory); Err(unsupported())
    }
}

pub(super) fn directory(path: &Path) -> io::Result<File> {
    namespace::validate_absolute(path)?;
    #[cfg(any(target_vendor = "apple", target_os = "linux"))] {
        let mut held = File::open("/")?;
        for component in path.components() {
            match component {
                std::path::Component::RootDir => (),
                std::path::Component::Normal(value) => held = child(&held, value, true)?,
                _ => return Err(invalid("directory traversal requires an exact absolute path")),
            }
        }
        Ok(held)
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))] { let _ = path; Err(unsupported()) }
}

pub(super) fn pinned_directory(path: &Path, identity: &FileIdentity) -> io::Result<File> {
    let directory = directory(path)?;
    if FileIdentity::of(&directory.metadata()?)? != *identity {
        return Err(invalid("descriptor-relative directory differs from its held identity"));
    }
    Ok(directory)
}

pub(super) fn state(parent: &File, value: &OsStr) -> io::Result<NodeState> {
    #[cfg(any(target_vendor = "apple", target_os = "linux"))] {
        use std::os::fd::AsRawFd;
        let name = name(value)?;
        let mut state = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstatat(parent.as_raw_fd(), name.as_ptr(), state.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let state = unsafe { state.assume_init() };
        let kind = match state.st_mode & libc::S_IFMT {
            libc::S_IFDIR => DirectoryEntryKind::Directory,
            libc::S_IFREG => DirectoryEntryKind::File,
            libc::S_IFLNK => DirectoryEntryKind::Symlink,
            _ => return Err(invalid("directory publication rejects special filesystem entries")),
        };
        Ok(NodeState { identity: FileIdentity { device: state.st_dev as u64, inode: state.st_ino as u64 }, kind,
            mode: state.st_mode as u32 & 0o7777, bytes: u64::try_from(state.st_size).map_err(|_| invalid("filesystem entry has negative size"))?,
            modified: (state.st_mtime as i64, state.st_mtime_nsec as i64), changed: (state.st_ctime as i64, state.st_ctime_nsec as i64) })
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))] { let _ = (parent, value); Err(unsupported()) }
}

pub(super) fn link_target(parent: &File, value: &OsStr) -> io::Result<Vec<u8>> {
    #[cfg(any(target_vendor = "apple", target_os = "linux"))] {
        use std::os::fd::AsRawFd;
        let name = name(value)?;
        let mut bytes = vec![0u8; 256];
        loop {
            let count = unsafe { libc::readlinkat(parent.as_raw_fd(), name.as_ptr(), bytes.as_mut_ptr().cast(), bytes.len()) };
            if count < 0 { return Err(io::Error::last_os_error()); }
            if (count as usize) < bytes.len() { bytes.truncate(count as usize); return Ok(bytes); }
            if bytes.len() >= RECORD_LIMIT as usize { return Err(invalid("symlink target exceeds directory publication bound")); }
            bytes.resize(bytes.len() * 2, 0);
        }
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))] { let _ = (parent, value); Err(unsupported()) }
}

pub(super) fn names(directory: &File, limit: usize) -> io::Result<Vec<OsString>> {
    #[cfg(any(target_vendor = "apple", target_os = "linux"))] {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStringExt;
        struct Stream(*mut libc::DIR);
        impl Drop for Stream { fn drop(&mut self) { unsafe { libc::closedir(self.0); } } }
        let fd = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        let pointer = unsafe { libc::fdopendir(fd) };
        if pointer.is_null() {
            let error = io::Error::last_os_error();
            unsafe { libc::close(fd); }
            return Err(error);
        }
        let stream = Stream(pointer);
        unsafe { libc::rewinddir(stream.0); }
        let mut names = Vec::new();
        loop {
            #[cfg(target_vendor = "apple")] let errno = unsafe { libc::__error() };
            #[cfg(target_os = "linux")] let errno = unsafe { libc::__errno_location() };
            unsafe { *errno = 0; }
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                if unsafe { *errno } != 0 { return Err(io::Error::last_os_error()); }
                break;
            }
            let value = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if value == b"." || value == b".." { continue; }
            names.push(OsString::from_vec(value.to_vec()));
            if names.len() > limit { return Err(invalid("directory membership exceeds its manifest bound")); }
        }
        names.sort();
        Ok(names)
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))] { let _ = (directory, limit); Err(unsupported()) }
}
