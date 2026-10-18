//! Byte transport to the coordinator: a serial device or a network adapter.
//!
//! `Port` is the one type the ASH/EZSP layers read and write; it hides
//! whether the dongle is on `/dev/ttyUSB0` or behind `tcp://host:6638`.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::Context as _;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use crate::Config;
#[cfg(unix)]
use crate::Flow;

/// How long a network adapter gets to accept the connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// An open link to the coordinator.
#[derive(Debug)]
pub enum Port {
    #[cfg(unix)]
    Serial(SerialPort),
    Tcp(TcpStream),
}

/// Opens the link named by `config.port`: `tcp://host:port` for a network
/// adapter, anything else is a serial device path.
pub async fn open(config: &Config) -> anyhow::Result<Port> {
    if let Some(addr) = config.port.strip_prefix("tcp://") {
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .with_context(|| format!("connecting to {addr}: timed out"))?
            .with_context(|| format!("connecting to {addr}"))?;
        // EZSP frames are small and latency-bound: never wait to coalesce them.
        stream.set_nodelay(true).context("tcp nodelay")?;
        return Ok(Port::Tcp(stream));
    }
    open_serial(config)
}

#[cfg(unix)]
fn open_serial(config: &Config) -> anyhow::Result<Port> {
    SerialPort::open(&config.port, config.baud, config.flow).map(Port::Serial)
}

#[cfg(not(unix))]
fn open_serial(config: &Config) -> anyhow::Result<Port> {
    anyhow::bail!(
        "serial port {}: serial ports are not supported on this platform",
        config.port
    )
}

impl AsyncRead for Port {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Port::Serial(s) => Pin::new(s).poll_read(cx, buf),
            Port::Tcp(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Port {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            #[cfg(unix)]
            Port::Serial(s) => Pin::new(s).poll_write(cx, buf),
            Port::Tcp(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Port::Serial(s) => Pin::new(s).poll_flush(cx),
            Port::Tcp(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Port::Serial(s) => Pin::new(s).poll_shutdown(cx),
            Port::Tcp(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// A serial device in raw 8N1 mode, driven through the tokio reactor.
#[cfg(unix)]
#[derive(Debug)]
pub struct SerialPort {
    // The `File` owns the descriptor and closes it on drop.
    fd: tokio::io::unix::AsyncFd<std::fs::File>,
}

#[cfg(unix)]
impl SerialPort {
    /// Opens `path` non-blocking, without becoming its controlling terminal,
    /// and applies a raw termios configuration.
    ///
    /// Must be called inside a tokio runtime.
    pub fn open(path: &str, baud: u32, flow: Flow) -> anyhow::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;

        let speed = speed_constant(baud)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(path)
            .with_context(|| format!("opening serial port {path}"))?;
        configure(&file, speed, flow).with_context(|| format!("configuring serial port {path}"))?;
        let fd = tokio::io::unix::AsyncFd::new(file)
            .with_context(|| format!("registering serial port {path}"))?;
        Ok(Self { fd })
    }
}

/// The termios speed constant of a baud rate the dongles use.
#[cfg(unix)]
fn speed_constant(baud: u32) -> anyhow::Result<libc::speed_t> {
    Ok(match baud {
        9_600 => libc::B9600,
        19_200 => libc::B19200,
        38_400 => libc::B38400,
        57_600 => libc::B57600,
        115_200 => libc::B115200,
        230_400 => libc::B230400,
        460_800 => libc::B460800,
        921_600 => libc::B921600,
        other => anyhow::bail!("unsupported baud rate {other}"),
    })
}

/// Raw mode, 8N1, receiver on, modem lines ignored, then the flow control.
// libc has no safe termios: this is the only unsafe code of the crate.
#[cfg(unix)]
#[allow(unsafe_code)]
fn configure(file: &std::fs::File, speed: libc::speed_t, flow: Flow) -> io::Result<()> {
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd;

    let fd = file.as_raw_fd();
    let mut tio = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `fd` is a valid open descriptor owned by `file`; `tio` is a
    // writable termios, and it is only read after tcgetattr reported success.
    let mut tio = unsafe {
        if libc::tcgetattr(fd, tio.as_mut_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }
        tio.assume_init()
    };
    // SAFETY: `tio` is an initialised termios and `speed` a valid constant.
    unsafe {
        libc::cfmakeraw(&raw mut tio);
        if libc::cfsetispeed(&raw mut tio, speed) != 0
            || libc::cfsetospeed(&raw mut tio, speed) != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    tio.c_cflag |= libc::CLOCAL | libc::CREAD | libc::CS8;
    tio.c_cflag &= !(libc::PARENB | libc::CSTOPB | libc::CRTSCTS);
    tio.c_iflag &= !(libc::IXON | libc::IXOFF | libc::IXANY);
    match flow {
        Flow::Hardware => tio.c_cflag |= libc::CRTSCTS,
        Flow::Software => tio.c_iflag |= libc::IXON | libc::IXOFF,
        Flow::None => {}
    }
    tio.c_cc[libc::VMIN] = 1;
    tio.c_cc[libc::VTIME] = 0;
    // SAFETY: valid descriptor and an initialised termios, as above.
    unsafe {
        // Whatever the previous user left in the queues is not ours to read.
        libc::tcflush(fd, libc::TCIOFLUSH);
        if libc::tcsetattr(fd, libc::TCSANOW, &raw const tio) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(unix)]
impl AsyncRead for SerialPort {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        use std::io::Read;

        let this = self.get_mut();
        loop {
            let mut guard = std::task::ready!(this.fd.poll_read_ready(cx))?;
            let read = guard.try_io(|inner| inner.get_ref().read(buf.initialize_unfilled()));
            match read {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                // Readiness was a false alarm: wait for the next one.
                Err(_would_block) => {}
            }
        }
    }
}

#[cfg(unix)]
impl AsyncWrite for SerialPort {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        use std::io::Write;

        let this = self.get_mut();
        loop {
            let mut guard = std::task::ready!(this.fd.poll_write_ready(cx))?;
            if let Ok(result) = guard.try_io(|inner| inner.get_ref().write(buf)) {
                return Poll::Ready(result);
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Writes go straight to the kernel queue: nothing is buffered here.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // The descriptor closes when the port is dropped.
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Flow;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn config(port: &str, baud: u32, flow: Flow) -> Config {
        Config {
            port: port.to_string(),
            baud,
            flow,
            channel: None,
        }
    }

    #[tokio::test]
    async fn tcp_round_trip() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).await.unwrap();
            s.write_all(&buf).await.unwrap();
            // Keep the socket open until the client is done.
            let _ = s.read(&mut buf).await;
        });
        let mut port = open(&config(&format!("tcp://{addr}"), 115_200, Flow::None))
            .await
            .unwrap();
        port.write_all(&[0x1a, 0xc0, 0x38, 0xbc]).await.unwrap();
        let mut back = [0u8; 4];
        port.read_exact(&mut back).await.unwrap();
        assert_eq!(back, [0x1a, 0xc0, 0x38, 0xbc]);
    }

    #[tokio::test]
    async fn peer_close_is_an_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            drop(s);
        });
        let mut port = open(&config(&format!("tcp://{addr}"), 115_200, Flow::None))
            .await
            .unwrap();
        let mut buf = [0u8; 8];
        // A closed peer reads as end of stream: the caller turns it into an error.
        assert_eq!(port.read(&mut buf).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn tcp_connection_refused_names_the_address() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let err = open(&config(&format!("tcp://{addr}"), 115_200, Flow::None))
            .await
            .unwrap_err();
        assert!(err.to_string().contains(&addr.to_string()), "{err:#}");
    }

    #[tokio::test]
    async fn missing_serial_port_errors() {
        let err = open(&config("/dev/does-not-exist", 115_200, Flow::Software))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("/dev/does-not-exist"), "{err:#}");
    }

    #[tokio::test]
    async fn unsupported_baud_names_the_rate() {
        let err = open(&config("/dev/does-not-exist", 12_345, Flow::Software))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("12345"), "{err:#}");
    }

    /// The serial path against a pseudo-terminal: the same termios calls as a
    /// real dongle, without the hardware.
    #[cfg(unix)]
    #[allow(unsafe_code)] // pty plumbing; libc only.
    mod pty {
        use super::*;
        use std::fs::File;
        use std::io::{Read, Write};
        use std::os::fd::{AsRawFd, FromRawFd};

        /// A pty pair: the master end as a file, the slave device path.
        fn pty() -> (File, String) {
            // SAFETY: plain libc calls on a descriptor checked right after
            // creation; `ptsname_r` writes at most `name.len()` bytes into `name`.
            unsafe {
                let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
                assert!(master >= 0, "posix_openpt: {}", io::Error::last_os_error());
                assert_eq!(libc::grantpt(master), 0);
                assert_eq!(libc::unlockpt(master), 0);
                let mut name = [0 as libc::c_char; 128];
                assert_eq!(libc::ptsname_r(master, name.as_mut_ptr(), name.len()), 0);
                let path = std::ffi::CStr::from_ptr(name.as_ptr())
                    .to_str()
                    .unwrap()
                    .to_string();
                (File::from_raw_fd(master), path)
            }
        }

        fn termios_of(file: &File) -> libc::termios {
            // SAFETY: valid descriptor; the termios is read only after success.
            unsafe {
                let mut tio = std::mem::MaybeUninit::<libc::termios>::uninit();
                assert_eq!(libc::tcgetattr(file.as_raw_fd(), tio.as_mut_ptr()), 0);
                tio.assume_init()
            }
        }

        #[tokio::test]
        async fn serial_round_trip() {
            let (mut master, path) = pty();
            let mut port = open(&config(&path, 115_200, Flow::Software)).await.unwrap();

            // Device to host: bytes written to the master end arrive raw
            // (no 0x11/0x13: with software flow control the tty keeps those).
            master.write_all(&[0x1a, 0xc0, 0x38, 0xbc, 0x7e]).unwrap();
            let mut got = [0u8; 5];
            port.read_exact(&mut got).await.unwrap();
            assert_eq!(got, [0x1a, 0xc0, 0x38, 0xbc, 0x7e]);

            // Host to device.
            port.write_all(&[0x81, 0x60, 0x59]).await.unwrap();
            port.flush().await.unwrap();
            let mut back = [0u8; 3];
            master.read_exact(&mut back).unwrap();
            assert_eq!(back, [0x81, 0x60, 0x59]);
        }

        #[tokio::test]
        async fn flow_control_follows_the_config() {
            for (flow, xon, rtscts) in [
                (Flow::Software, true, false),
                (Flow::Hardware, false, true),
                (Flow::None, false, false),
            ] {
                let (_master, path) = pty();
                let port = open(&config(&path, 115_200, flow)).await.unwrap();
                let Port::Serial(serial) = &port else {
                    panic!("expected a serial port");
                };
                let tio = termios_of(serial.fd.get_ref());
                assert_eq!(
                    tio.c_iflag & (libc::IXON | libc::IXOFF) != 0,
                    xon,
                    "{flow:?}"
                );
                assert_eq!(tio.c_cflag & libc::CRTSCTS != 0, rtscts, "{flow:?}");
                assert_eq!(tio.c_cflag & libc::CSIZE, libc::CS8);
                assert_eq!(tio.c_cflag & (libc::PARENB | libc::CSTOPB), 0);
                assert_eq!(tio.c_oflag & libc::OPOST, 0);
                assert_eq!(tio.c_lflag & (libc::ICANON | libc::ECHO), 0);
            }
        }
    }
}
