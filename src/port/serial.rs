// Copyright (C) 2026 Jared Crapo
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! `serial`: a TNC on a USB or UART serial device, such as a TH-D75 on
//! its USB cable, a NinoTNC, or a TNC-Pi.
//!
//! Each connect opens the device 8N1 with no flow control, takes it
//! exclusively (`TIOCEXCL` and `flock`), and runs the TNC's KISS init. A missing path
//! (cable unplugged, radio off) is reported as "waiting for device", which
//! the bridge polls every couple of seconds; udev events can replace the
//! polling later without a libudev dependency today.

use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_serial::{DataBits, FlowControl, Parity, SerialPortBuilderExt, SerialStream, StopBits};

use crate::bridge::{ConnectFuture, Opened, TncConnector};
use crate::config::{SerialDevice, TncModel};
use crate::error::Error;
use crate::port::tnc_init::{self, Prefixed};

pub struct SerialConnector {
    tnc_id: String,
    device: SerialDevice,
    model: TncModel,
    exit_kiss_on_disconnect: bool,
}

pub fn connector(
    tnc_id: String,
    device: SerialDevice,
    model: TncModel,
    exit_kiss_on_disconnect: bool,
) -> SerialConnector {
    SerialConnector {
        tnc_id,
        device,
        model,
        exit_kiss_on_disconnect,
    }
}

/// A serial port whose flush doesn't wait for the device.
///
/// tokio-serial's flush is `tcdrain()`, a blocking call that returns once
/// the device has taken every byte written. A TNC that stops reading (a
/// TH-D75 did, after its USB cable was plugged back in) leaves it blocked
/// for good, holding a runtime thread, with no timeout able to fire.
/// Bytes written are already in the kernel's queue, which is all the bridge
/// needs; a device that doesn't take them shows up as writes that don't
/// complete, which async code can time out.
struct NoDrain(SerialStream);

/// Unsent output is discarded on close. Otherwise Linux holds `close()` for
/// up to 30 s (the tty's closing wait) trying to deliver it to a device
/// that isn't reading, which is exactly when the bridge closes the port to
/// try again.
#[cfg(unix)]
impl Drop for NoDrain {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // SAFETY: the fd is open until the SerialStream drops, after this.
        unsafe { libc::tcflush(self.0.as_raw_fd(), libc::TCOFLUSH) };
    }
}

impl AsyncRead for NoDrain {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for NoDrain {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl TncConnector for SerialConnector {
    fn connect(&self) -> ConnectFuture {
        let tnc_id = self.tnc_id.clone();
        let SerialDevice { path, baud } = self.device.clone();
        let model = self.model;
        let exit_kiss = self.exit_kiss_on_disconnect;
        Box::pin(async move {
            let absent = || {
                Error::DeviceAbsent(format!(
                    "tnc {tnc_id}: {path} isn't present (cable unplugged, or the radio is off?)"
                ))
            };
            if !Path::new(&path).exists() {
                return Err(absent());
            }
            let port = tokio_serial::new(&path, baud)
                .data_bits(DataBits::Eight)
                .parity(Parity::None)
                .stop_bits(StopBits::One)
                .flow_control(FlowControl::None)
                // TIOCEXCL plus an exclusive flock: nobody else opens it
                // while the bridge holds it.
                .exclusive(true)
                .open_native_async()
                .map_err(|e| match e.kind {
                    tokio_serial::ErrorKind::Io(std::io::ErrorKind::NotFound) => absent(),
                    // serialport's name for EBUSY: someone else holds it
                    // exclusively.
                    tokio_serial::ErrorKind::NoDevice => {
                        Error::Connect(format!("tnc {tnc_id}: {path} is in use by another program"))
                    }
                    _ => Error::Connect(format!("tnc {tnc_id}: can't open {path}: {e}")),
                })?;
            tracing::debug!(tnc = %tnc_id, path, baud, "serial device open");
            let mut port = NoDrain(port);
            let out = tnc_init::init(&mut port, model, exit_kiss).await?;
            Ok(Opened {
                link: Box::new(Prefixed::new(port, &out.frames)),
                teardown: out.teardown,
            })
        })
    }

    fn describe(&self) -> String {
        format!("tnc {} on {}", self.tnc_id, self.device.path)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::port::stream::KissConnection;
    #[cfg(target_os = "linux")]
    use std::os::fd::{FromRawFd, OwnedFd};

    /// A pty pair: the master end for the test, the slave's path for the
    /// connector to open.
    #[cfg(target_os = "linux")]
    fn pty() -> (OwnedFd, String) {
        let mut master = -1;
        let mut slave = -1;
        let mut name = [0 as libc::c_char; 128];
        // SAFETY: openpty fills in two fds and a NUL-terminated name that
        // fits a buffer this size.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                name.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
        // SAFETY: both fds were just opened and are owned here.
        let (master, _slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // SAFETY: openpty wrote a NUL-terminated string.
        let path = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        (master, path)
    }

    #[tokio::test]
    async fn missing_device_is_waiting_not_failing() {
        let c = connector(
            "nino".into(),
            SerialDevice {
                path: "/dev/serial/by-id/nothing-here".into(),
                baud: 57600,
            },
            TncModel::Generic,
            false,
        );
        let err = c.connect().await.err().expect("should fail");
        assert!(matches!(err, Error::DeviceAbsent(_)), "{err:?}");
        assert!(err.to_string().contains("isn't present"), "{err}");
    }

    // macOS ptys and qemu-user (cross test) refuse serialport's termios
    // ioctls with ENOTTY, so this runs on a native Linux kernel only.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn opens_by_path_exclusively_and_carries_kiss() {
        let (master, path) = pty();
        let c = connector(
            "nino".into(),
            SerialDevice {
                path: path.clone(),
                baud: 57600,
            },
            TncModel::Generic,
            false,
        );
        assert_eq!(c.describe(), format!("tnc nino on {path}"));
        let opened = match c.connect().await {
            Err(e) if e.to_string().contains("Not a typewriter") => {
                eprintln!("skipped: this environment's ptys refuse termios ioctls ({e})");
                return;
            }
            other => other.unwrap(),
        };
        assert!(opened.teardown.is_empty());

        // Someone else opening it now gets EBUSY, unless they're root.
        if unsafe { libc::geteuid() } != 0 {
            let again = c.connect().await.err().expect("second open should fail");
            assert!(
                again.to_string().contains("in use by another program"),
                "{again:?}"
            );
        }

        let master = tokio::fs::File::from_std(std::fs::File::from(master));
        let mut tnc = KissConnection::new(master);
        let mut link = KissConnection::new(opened.link);
        tnc.write_frame(&[0x00, 0x11]).await.unwrap();
        assert_eq!(link.read_frame().await.unwrap(), vec![0x00, 0x11]);
        link.write_frame(&[0x00, 0x22]).await.unwrap();
        assert_eq!(tnc.read_frame().await.unwrap(), vec![0x00, 0x22]);
    }
}
