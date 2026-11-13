# AttestationResponse byte-parity fixtures

Emitted by `cargo xtask capture-attestation`. See
`xtask/scripts/capture-attestation/index.js` for capture mechanics.

## Files

- `happy-path.json` — one matter.js-signed (`elements`, `challenge`,
  `pubkey`, `signature`) tuple plus four single-byte mutations,
  each cross-verified to reject under matter.js's
  `NodeJsStyleCrypto.verifyEcdsa`.

## Byte-parity claim

For the same tuple, `crates/matter-commissioning::verify_attestation_response`
and matter.js's verifier produce the same accept/reject verdict.
Asserted by `crates/matter-commissioning/tests/attestation_response_byte_parity.rs`.

## Regenerating

```bash
cd xtask/scripts/capture-attestation && npm install
cd -
cargo xtask capture-attestation
```

Re-running the script mints a fresh keypair and a fresh signature
(ECDSA's `k` is randomized per signing call), so the fixture file
changes byte-for-byte on each run. The verdict matrix
(accept + 4 reject mutations) does not. Commit the regenerated
fixture if you re-run.

## License

The capture script and its output are Apache-2.0 (matches the
workspace license). matter.js, the upstream provider of
`@matter/general`, is also Apache-2.0.
