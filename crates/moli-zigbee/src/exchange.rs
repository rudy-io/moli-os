//! Requests and their answers: a device's ZDO or ZCL response comes back as
//! an incoming message, matched here to the request that awaits it.
//!
//! Whoever reads the callbacks hands every incoming message to
//! [`Exchanges::deliver`]; a request never waits on the loop that reads them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::oneshot;

use crate::aps::{Aps, ApsFrame, DEFAULT_OPTIONS, Incoming};
use crate::coordinator::ENDPOINT;
use crate::{zcl, zdo};

/// How long a device gets to answer once it has the request (herdsman's
/// default for ZCL).
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// What a response is matched by. A short address, the cluster and the
/// transaction number: two requests to one device never share all three.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    nwk: u16,
    zdo: bool,
    cluster: u16,
    seq: u8,
}

#[derive(Debug, Default)]
struct Inner {
    waiting: HashMap<Key, oneshot::Sender<Vec<u8>>>,
    /// The last ZCL transaction number used.
    seq: u8,
}

/// The requests awaiting an answer; clones share them.
#[derive(Clone, Debug, Default)]
pub struct Exchanges {
    inner: Arc<Mutex<Inner>>,
}

impl Exchanges {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The next ZCL transaction number.
    #[must_use]
    pub fn next_seq(&self) -> u8 {
        let mut inner = self.lock();
        inner.seq = inner.seq.wrapping_add(1);
        inner.seq
    }

    fn expect(&self, key: Key) -> Expected {
        let (sender, receiver) = oneshot::channel();
        self.lock().waiting.insert(key, sender);
        Expected {
            exchanges: self.clone(),
            key,
            receiver,
        }
    }

    /// Hands `message` to the request it answers, if any: `true` when one
    /// took it. A report or a device's own command never answers one, even
    /// with the same number.
    pub fn deliver(&self, message: &Incoming) -> bool {
        let frame = &message.frame;
        let key = if frame.profile == zdo::PROFILE {
            let Some(seq) = zdo::transaction(&message.payload) else {
                return false;
            };
            Key {
                nwk: message.nwk,
                zdo: true,
                cluster: frame.cluster,
                seq,
            }
        } else {
            let Ok((header, _)) = zcl::Header::parse(&message.payload) else {
                return false;
            };
            let response = matches!(
                header.command,
                zcl::READ_RESPONSE
                    | zcl::WRITE_RESPONSE
                    | zcl::CONFIGURE_RESPONSE
                    | zcl::DEFAULT_RESPONSE
            );
            if header.specific || !response {
                return false;
            }
            Key {
                nwk: message.nwk,
                zdo: false,
                cluster: frame.cluster,
                seq: header.seq,
            }
        };
        let waiter = self.lock().waiting.remove(&key);
        waiter.is_some_and(|waiter| waiter.send(message.payload.clone()).is_ok())
    }
}

/// An answer awaited; released however the wait ends.
#[derive(Debug)]
pub struct Expected {
    exchanges: Exchanges,
    key: Key,
    receiver: oneshot::Receiver<Vec<u8>>,
}

impl Expected {
    /// The answer's payload, within `timeout`.
    ///
    /// # Errors
    ///
    /// No answer in time.
    pub async fn wait(mut self, timeout: Duration) -> anyhow::Result<Vec<u8>> {
        match tokio::time::timeout(timeout, &mut self.receiver).await {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(_)) => anyhow::bail!("the answer was taken back"),
            Err(_) => anyhow::bail!(
                "no answer from {:#06x} (cluster {:#06x}) within {timeout:?}",
                self.key.nwk,
                self.key.cluster
            ),
        }
    }
}

impl Drop for Expected {
    fn drop(&mut self) {
        self.exchanges.lock().waiting.remove(&self.key);
    }
}

/// The way to a device: send a request, wait for its answer.
#[derive(Clone, Debug)]
pub struct Link {
    pub aps: Aps,
    pub exchanges: Exchanges,
}

/// The APS frame of a ZCL frame from the coordinator's endpoint.
#[must_use]
pub fn zcl_frame(endpoint: u8, cluster: u16) -> ApsFrame {
    ApsFrame {
        profile: zcl::HA_PROFILE,
        cluster,
        src_ep: ENDPOINT.endpoint,
        dst_ep: endpoint,
        options: DEFAULT_OPTIONS,
        group: 0,
        seq: 0,
    }
}

impl Link {
    #[must_use]
    pub fn new(aps: Aps) -> Self {
        Self {
            aps,
            exchanges: Exchanges::default(),
        }
    }

    /// A ZDO request to `nwk`, `build` writing its payload with the
    /// transaction number; the payload of its response.
    ///
    /// # Errors
    ///
    /// Not delivered, or no answer in time.
    pub async fn zdo(
        &self,
        nwk: u16,
        cluster: u16,
        response: u16,
        build: impl FnOnce(u8) -> Vec<u8> + Send,
    ) -> anyhow::Result<Vec<u8>> {
        let seq = self.aps.next_tag();
        let expected = self.exchanges.expect(Key {
            nwk,
            zdo: true,
            cluster: response,
            seq,
        });
        let sent = self
            .aps
            .send_unicast_tagged(nwk, &zdo::frame(cluster), seq, &build(seq))
            .await;
        answer(sent, expected).await
    }

    /// Registers the answer to a ZCL request about to be sent; its frame.
    #[must_use]
    pub fn zcl_request(
        &self,
        nwk: u16,
        cluster: u16,
        request: &zcl::Request,
    ) -> (Expected, Vec<u8>) {
        let seq = self.exchanges.next_seq();
        let expected = self.exchanges.expect(Key {
            nwk,
            zdo: false,
            cluster,
            seq,
        });
        (expected, request.frame(seq))
    }

    /// A ZCL request to `endpoint` of `nwk`; the whole frame that answers it.
    ///
    /// # Errors
    ///
    /// Not delivered, or no answer in time.
    pub async fn zcl(
        &self,
        nwk: u16,
        endpoint: u8,
        cluster: u16,
        request: &zcl::Request,
    ) -> anyhow::Result<Vec<u8>> {
        let (expected, frame) = self.zcl_request(nwk, cluster, request);
        let sent = self
            .aps
            .send_unicast(nwk, &zcl_frame(endpoint, cluster), &frame)
            .await;
        answer(sent, expected).await
    }

    /// Reads attributes, and what they answered with: nothing, from a device
    /// that says with a default response that it does not read these. An
    /// error is always a silence.
    ///
    /// # Errors
    ///
    /// Not delivered, or no answer in time.
    pub async fn read(
        &self,
        nwk: u16,
        endpoint: u8,
        cluster: u16,
        attributes: &[u16],
    ) -> anyhow::Result<Vec<zcl::Attribute>> {
        let frame = self
            .zcl(nwk, endpoint, cluster, &zcl::read_attributes(attributes))
            .await?;
        Ok(match zcl::Header::parse(&frame) {
            Ok((header, payload)) if header.command == zcl::READ_RESPONSE => {
                zcl::read_response(payload).0
            }
            _ => Vec::new(),
        })
    }
}

/// The answer to what was just sent. One already there counts even if the
/// report of its delivery failed or came late: it beat it.
async fn answer(sent: anyhow::Result<()>, expected: Expected) -> anyhow::Result<Vec<u8>> {
    match sent {
        Ok(()) => expected.wait(RESPONSE_TIMEOUT).await,
        Err(error) => expected.wait(Duration::ZERO).await.map_err(|_| error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(nwk: u16, profile: u16, cluster: u16, payload: &[u8]) -> Incoming {
        Incoming {
            nwk,
            frame: ApsFrame {
                profile,
                cluster,
                ..ApsFrame::default()
            },
            lqi: 200,
            broadcast: false,
            payload: payload.to_vec(),
        }
    }

    fn key(nwk: u16, zdo: bool, cluster: u16, seq: u8) -> Key {
        Key {
            nwk,
            zdo,
            cluster,
            seq,
        }
    }

    #[tokio::test]
    async fn a_response_finds_its_request() {
        let exchanges = Exchanges::default();
        let expected = exchanges.expect(key(0x1234, false, 0x0006, 7));
        // A read response from the server, its number.
        let answer = [0x18, 7, zcl::READ_RESPONSE, 0, 0, 0, 0x10, 1];
        // Another device, another cluster, another number: not it.
        assert!(!exchanges.deliver(&message(0x9999, 0x0104, 0x0006, &answer)));
        assert!(!exchanges.deliver(&message(0x1234, 0x0104, 0x0008, &answer)));
        let mut other = answer;
        other[1] = 8;
        assert!(!exchanges.deliver(&message(0x1234, 0x0104, 0x0006, &other)));
        assert!(exchanges.deliver(&message(0x1234, 0x0104, 0x0006, &answer)));
        assert_eq!(expected.wait(RESPONSE_TIMEOUT).await.unwrap(), answer);
    }

    #[tokio::test]
    async fn a_report_never_answers_a_request() {
        let exchanges = Exchanges::default();
        let expected = exchanges.expect(key(0x1234, false, 0x0006, 7));
        // A report with the same number, then a device's own command.
        let report = [0x18, 7, zcl::REPORT_ATTRIBUTES, 0, 0, 0x10, 1];
        assert!(!exchanges.deliver(&message(0x1234, 0x0104, 0x0006, &report)));
        let command = [0x19, 7, zcl::READ_RESPONSE];
        assert!(!exchanges.deliver(&message(0x1234, 0x0104, 0x0006, &command)));
        // A frame too short to read.
        assert!(!exchanges.deliver(&message(0x1234, 0x0104, 0x0006, &[0x18])));
        // A default response does answer it.
        let refusal = [0x18, 7, zcl::DEFAULT_RESPONSE, 0x01, 0x86];
        assert!(exchanges.deliver(&message(0x1234, 0x0104, 0x0006, &refusal)));
        assert_eq!(expected.wait(RESPONSE_TIMEOUT).await.unwrap(), refusal);
    }

    #[tokio::test]
    async fn zdo_responses_are_matched_apart_from_zcl() {
        let exchanges = Exchanges::default();
        let zdo_expected = exchanges.expect(key(0x1234, true, zdo::NODE_DESCRIPTOR_RESPONSE, 3));
        assert!(!exchanges.deliver(&message(
            0x1234,
            zdo::PROFILE,
            zdo::ACTIVE_ENDPOINTS_RESPONSE,
            &[3, 0],
        )));
        assert!(!exchanges.deliver(&message(0x1234, zdo::PROFILE, 0x8002, &[])));
        assert!(exchanges.deliver(&message(0x1234, zdo::PROFILE, 0x8002, &[3, 0])));
        assert_eq!(
            zdo_expected.wait(RESPONSE_TIMEOUT).await.unwrap(),
            [3, 0],
            "the whole payload"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn no_answer_times_out_and_lets_go() {
        let exchanges = Exchanges::default();
        let expected = exchanges.expect(key(0x1234, false, 0x0006, 7));
        let error = expected.wait(RESPONSE_TIMEOUT).await.unwrap_err();
        assert!(error.to_string().contains("no answer"), "{error}");
        assert!(exchanges.lock().waiting.is_empty(), "released");
        // Given up before the answer: released too.
        drop(exchanges.expect(key(0x1234, false, 0x0006, 8)));
        assert!(exchanges.lock().waiting.is_empty());
    }

    #[test]
    fn transaction_numbers_go_round() {
        let exchanges = Exchanges::default();
        assert_eq!(exchanges.next_seq(), 1);
        exchanges.lock().seq = 255;
        assert_eq!(exchanges.next_seq(), 0);
    }
}
