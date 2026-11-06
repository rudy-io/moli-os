//! In-process BDX loopback: a minimal test *receiver* drives the
//! receiver-driven `BlockSender` (`ReceiveInit` -> `ReceiveAccept` ->
//! `BlockQuery`\* -> `Block`\*/`BlockEOF` -> `BlockAckEOF`) and asserts the
//! reassembled bytes match the served image. This is the F2 integration
//! validation; F4 upgrades the oracle to chip's live `ota-requestor-app`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use matter_bdx::{
    BdxMessage, BlockSender, CounterMessage, MessageType, OutgoingMessage, ReceiveAccept,
    SenderOutcome, TransferControl, TransferInit,
};

/// Drive a complete transfer of `image` with `provider_cap`/`receiver_cap`
/// block-size proposals; return the receiver's reassembled bytes.
fn run_transfer(image: &[u8], provider_cap: u16, receiver_cap: u16) -> Vec<u8> {
    let mut sender = BlockSender::new(image.to_vec(), provider_cap);

    // Receiver -> ReceiveInit.
    let init = TransferInit {
        control: TransferControl::RECEIVER_DRIVE,
        version: 0,
        max_block_size: receiver_cap,
        start_offset: 0,
        max_length: 0,
        file_designator: b"fw.ota".to_vec(),
        metadata: Vec::new(),
    };
    let SenderOutcome::Send(OutgoingMessage {
        message_type,
        payload,
    }) = sender.accept_receive_init(&init)
    else {
        panic!("expected ReceiveAccept");
    };
    assert_eq!(message_type, MessageType::ReceiveAccept);
    let accept = ReceiveAccept::decode(&payload).unwrap();
    assert_eq!(u64::try_from(image.len()).unwrap(), accept.length);

    // Receiver pulls blocks until BlockEOF, ack-ing the EOF.
    let mut reassembled = Vec::new();
    let mut counter = 0u32;
    loop {
        let outcome = sender.handle_block_query(&CounterMessage {
            block_counter: counter,
        });
        let SenderOutcome::Send(OutgoingMessage {
            message_type,
            payload,
        }) = outcome
        else {
            panic!("expected a block, got {outcome:?}");
        };
        // Decode via the public dispatch so the receiver path is exercised too.
        match BdxMessage::decode(message_type, &payload).unwrap() {
            BdxMessage::Block(b) => {
                reassembled.extend_from_slice(&b.data);
                counter = counter.wrapping_add(1);
            }
            BdxMessage::BlockEof(b) => {
                reassembled.extend_from_slice(&b.data);
                assert_eq!(
                    sender.handle_block_ack_eof(&CounterMessage {
                        block_counter: b.block_counter
                    }),
                    SenderOutcome::Done
                );
                break;
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }
    assert!(sender.is_complete());
    reassembled
}

#[test]
fn multi_block_short_final() {
    let image = b"HELLO WORLD, THIS IS A BDX IMAGE.".to_vec(); // 33 bytes
    assert_eq!(run_transfer(&image, 1024, 8), image); // 8-byte blocks: 4 full + 1 short
}

#[test]
fn exact_multiple_no_trailing_empty() {
    let image = b"ABCDEFGH".to_vec(); // 8 bytes, 4-byte blocks -> exactly 2
    assert_eq!(run_transfer(&image, 4, 4), image);
}

#[test]
fn single_block() {
    let image = b"tiny".to_vec();
    assert_eq!(run_transfer(&image, 1024, 1024), image);
}

#[test]
fn provider_cap_wins_when_smaller() {
    let image = vec![0xABu8; 100];
    // Provider caps at 16 even though receiver proposes 1024.
    assert_eq!(run_transfer(&image, 16, 1024), image);
}

#[test]
fn larger_realistic_image() {
    let image: Vec<u8> = (0..5000u32)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    assert_eq!(run_transfer(&image, 1024, 1024), image);
}
