//! Cancellable fd-3 bootstrap delivery, bounded by the instance startup timeout.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use tokio::io::unix::AsyncFd;

pub(super) struct BootstrapWriter {
    fd: OwnedFd,
    payload: Vec<u8>,
}

pub(super) fn pipe(payload: Vec<u8>) -> io::Result<(OwnedFd, BootstrapWriter)> {
    let mut fds = [0; 2];
    // SAFETY: fds has space for both descriptors returned by pipe.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe returned unique descriptors owned by this function.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    tako_spawn::set_cloexec(&write)?;
    // Only the parent writer is nonblocking; SDK readers retain blocking fd 3.
    let flags = unsafe { libc::fcntl(write.as_raw_fd(), libc::F_GETFL) };
    if flags == -1
        || unsafe { libc::fcntl(write.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok((read, BootstrapWriter { fd: write, payload }))
}

impl BootstrapWriter {
    pub(super) async fn write(self) -> io::Result<()> {
        let fd = AsyncFd::new(self.fd)?;
        let mut remaining = self.payload.as_slice();
        while !remaining.is_empty() {
            let mut ready = fd.writable().await?;
            match ready.try_io(|fd| {
                // SAFETY: remaining is live for this call and fd is owned.
                let written = unsafe {
                    libc::write(
                        fd.get_ref().as_raw_fd(),
                        remaining.as_ptr().cast(),
                        remaining.len(),
                    )
                };
                if written < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(written as usize)
                }
            }) {
                Ok(Ok(0)) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(Ok(n)) => remaining = &remaining[n..],
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(error)) => return Err(error),
                Err(_) => continue,
            }
        }
        Ok(())
    }
}
