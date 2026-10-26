//! One conversation with the dongle on TCP 51515: `@HP:` (code, our name,
//! the token it gave us), questions and their answers, then an empty frame
//! to end it. The dongle serves one client at a time, so a session is
//! short: J.O.E. finds it free between two.
//!
//! Answers are lower case (`@tg:43…`); what the machine pushes on its own
//! is upper case (`@TF:` every two seconds, `@TV:` while busy, `@TB`,
//! `@TS`) and is kept aside, never taken for an answer.

use std::time::Duration;

use anyhow::{Context, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout, timeout_at};

use crate::model::Status;
use crate::wire::{Frames, frame, hex, key};

pub const PORT: u16 = 51515;
const CONNECT: Duration = Duration::from_secs(4);
/// The dongle answers `@HP:` in well under a second; pairing waits for a
/// person at the machine (J.O.E. gives 40 s).
const HELLO: Duration = Duration::from_secs(8);
pub const PAIRING: Duration = Duration::from_secs(60);

/// How the dongle took us.
#[derive(Debug, PartialEq, Eq)]
pub enum Hello {
    Accepted,
    /// First pairing: the token to keep.
    Paired(String),
    /// A code is set on the machine and ours is missing or wrong.
    WrongPin,
    /// It does not know our token (machine reset, paired again elsewhere).
    Unknown,
}

#[derive(Debug)]
pub struct Session {
    stream: TcpStream,
    frames: Frames,
    seed: u64,
    /// The last status the machine pushed in this session.
    pub status: Option<Status>,
}

impl Session {
    /// Connects and says who we are. `token` empty: pairing (a person
    /// presses OK on the machine).
    pub async fn open(
        host: &str,
        port: u16,
        pin: &str,
        name: &str,
        token: &str,
    ) -> anyhow::Result<(Self, Hello)> {
        let stream = timeout(CONNECT, TcpStream::connect((host, port)))
            .await
            .context("jura: no answer")?
            .context("jura: unreachable")?;
        stream.set_nodelay(true).ok();
        let seed = u64::from(std::process::id())
            ^ std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| {
                    u64::try_from(d.as_nanos() & u128::from(u64::MAX)).unwrap_or(0)
                });
        let mut session = Self {
            stream,
            frames: Frames::default(),
            seed,
            status: None,
        };
        let name = hex(name.as_bytes());
        session.send(&format!("@HP:{pin},{name},{token}")).await?;
        let wait = if token.is_empty() { PAIRING } else { HELLO };
        let answer = session
            .answer("@hp", wait)
            .await?
            .context("jura: no answer to @HP")?;
        let hello = match answer.as_str() {
            "@hp4" => Hello::Accepted,
            a if a.starts_with("@hp4:") => Hello::Paired(a[5..].to_owned()),
            "@hp5" | "@hp5:00" => Hello::WrongPin,
            a if a.starts_with("@hp5") => Hello::Unknown,
            // Never the answer itself: it may carry a token.
            _ => bail!("jura: unexpected answer to @HP"),
        };
        Ok((session, hello))
    }

    async fn send(&mut self, text: &str) -> anyhow::Result<()> {
        self.seed = self.seed.wrapping_add(0x9E37_79B9);
        self.stream.write_all(&frame(text, key(self.seed))).await?;
        Ok(())
    }

    /// The next frame, or `None` once `until` has passed.
    async fn next(&mut self, until: Instant) -> anyhow::Result<Option<String>> {
        loop {
            if let Some(text) = self.frames.next() {
                return Ok(Some(text));
            }
            let mut buf = [0u8; 1024];
            match timeout_at(until, self.stream.read(&mut buf)).await {
                Err(_) => return Ok(None),
                Ok(Ok(0)) => bail!("jura: the dongle closed the session"),
                Ok(Ok(n)) => self.frames.push(&buf[..n]),
                Ok(Err(e)) => return Err(e.into()),
            }
        }
    }

    /// Waits for the answer starting with `prefix`; pushed frames meanwhile
    /// are kept (the status) or let go.
    async fn answer(&mut self, prefix: &str, wait: Duration) -> anyhow::Result<Option<String>> {
        let until = Instant::now() + wait;
        while let Some(text) = self.next(until).await? {
            if text.starts_with(prefix) || text.starts_with("@an:error") {
                return Ok(Some(text));
            }
            self.keep(&text);
        }
        Ok(None)
    }

    fn keep(&mut self, text: &str) {
        if let Some(status) = Status::parse(text) {
            self.status = Some(status);
        }
    }

    /// Asks `question`, returns the answer starting with `prefix` (or
    /// `@an:error`), `None` if none came in time.
    pub async fn ask(&mut self, question: &str, prefix: &str) -> anyhow::Result<Option<String>> {
        self.send(question).await?;
        self.answer(prefix, Duration::from_secs(4)).await
    }

    /// The status the machine pushes (every two seconds), waited for at
    /// most `wait`.
    pub async fn status(&mut self, wait: Duration) -> anyhow::Result<Option<&Status>> {
        let until = Instant::now() + wait;
        while self.status.is_none() {
            let Some(text) = self.next(until).await? else {
                break;
            };
            self.keep(&text);
        }
        Ok(self.status.as_ref())
    }

    /// Ends the session the app's way (an empty frame).
    pub async fn close(mut self) {
        let _ = self.send("").await;
        let _ = self.stream.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A dongle that answers a script: for each question, the frames to
    /// send back. Returns its port.
    async fn dongle(script: Vec<(&'static str, Vec<&'static str>)>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut frames = Frames::default();
            let mut buf = [0u8; 1024];
            for (question, answers) in script {
                let asked = loop {
                    if let Some(text) = frames.next() {
                        break text;
                    }
                    let n = stream.read(&mut buf).await.unwrap();
                    frames.push(&buf[..n]);
                };
                assert!(asked.starts_with(question), "{asked} for {question}");
                for a in answers {
                    stream.write_all(&frame(a, 0x42)).await.unwrap();
                }
            }
        });
        port
    }

    #[tokio::test]
    async fn pushed_frames_are_not_answers() {
        let port = dongle(vec![
            ("@HP:1234,4D6F6C69,abc", vec!["@TF:000400000C0000", "@hp4"]),
            (
                "@TG:43",
                vec!["@TB", "@TS", "@TV:3E28", "@tg:43000300030000004002110000"],
            ),
            ("@TP:", vec!["@tp:00"]),
        ])
        .await;
        let (mut s, hello) = Session::open("127.0.0.1", port, "1234", "Moli", "abc")
            .await
            .unwrap();
        assert_eq!(hello, Hello::Accepted);
        assert!(matches!(s.status, Some(Status::Idle(_))));
        let a = s.ask("@TG:43", "@tg:43").await.unwrap();
        assert_eq!(a.as_deref(), Some("@tg:43000300030000004002110000"));
        assert!(matches!(s.status, Some(Status::Busy { state: 0x3E, .. })));
        assert_eq!(
            s.ask("@TP:00", "@tp").await.unwrap().as_deref(),
            Some("@tp:00")
        );
        s.close().await;
    }

    #[tokio::test]
    async fn pairing_gives_a_token_and_a_code_can_be_wrong() {
        let port = dongle(vec![("@HP:,4D6F6C69,", vec!["@TF:00", "@hp4:ABCDEF"])]).await;
        let (_, hello) = Session::open("127.0.0.1", port, "", "Moli", "")
            .await
            .unwrap();
        assert_eq!(hello, Hello::Paired("ABCDEF".into()));
        let port = dongle(vec![("@HP:", vec!["@hp5:00"])]).await;
        let (_, hello) = Session::open("127.0.0.1", port, "", "Moli", "x")
            .await
            .unwrap();
        assert_eq!(hello, Hello::WrongPin);
        let port = dongle(vec![("@HP:", vec!["@hp5:01"])]).await;
        let (_, hello) = Session::open("127.0.0.1", port, "", "Moli", "x")
            .await
            .unwrap();
        assert_eq!(hello, Hello::Unknown);
    }
}
