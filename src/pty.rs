use std::io;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::task::ready;

use nix::fcntl::{OFlag, open};
use nix::pty::{PtyMaster, grantpt, posix_openpt, ptsname, unlockpt};
use nix::sys::stat::Mode;
use nix::sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub struct Pty {
    name: String,
    inner: Option<AsyncFd<PtyMaster>>,
    _keep_alive: OwnedFd,
}

impl Pty {
    pub fn new() -> io::Result<Pty> {
        // Create a new PTY
        let fd = posix_openpt(OFlag::O_RDWR | OFlag::O_NONBLOCK)?;

        // Allow a client to connect
        grantpt(&fd)?;
        unlockpt(&fd)?;

        // Set raw mode on the server side
        let mut termios = tcgetattr(&fd)?;
        cfmakeraw(&mut termios);
        tcsetattr(&fd, SetArg::TCSANOW, &termios)?;

        // Get the pty path
        let pty_name = unsafe { ptsname(&fd) }?;

        // Keep one client connection open at all times, to prevent hang-up signals from triggering
        // the tokio readiness
        let client = open(
            pty_name.as_str(),
            OFlag::O_RDWR | OFlag::O_NOCTTY,
            Mode::empty(),
        )?;

        Ok(Self {
            name: pty_name,
            inner: Some(AsyncFd::new(fd)?),
            _keep_alive: client,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl AsyncRead for Pty {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.inner.as_mut().unwrap().poll_read_ready(cx))?;

            let unfilled = buf.initialize_unfilled();

            match guard.try_io(|inner| inner.get_ref().read(unfilled)) {
                Ok(Ok(len)) => {
                    buf.advance(len);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(err)) => return Poll::Ready(Err(err)),
                Err(_would_block) => continue,
            }
        }
    }
}

impl AsyncWrite for Pty {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = ready!(self.inner.as_mut().unwrap().poll_write_ready(cx))?;

            match guard.try_io(|inner| inner.get_ref().write(buf)) {
                Ok(result) => return Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
