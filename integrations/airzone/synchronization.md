# Continuous Airzone state and command confirmation

The Local API driver polls; the manufacturer does not document a local event
subscription. Cloud consultation is separate and never grants local write access.

## Observation path

`POST /webserver` verifies gateway identity, then HVAC observations enter the
Hub before optional metadata reads. The Hub emits changed values through SSE.
Successful unchanged reads emit an Airzone receipt/generation heartbeat, because
the generic Hub deliberately does not emit a value-change event for equal values.
The dashboard applies those events without a full page reload.

Each observed entity carries `sync_last_received`, `sync_generation` and its
expected `sync_poll_interval`. Gateway telemetry additionally exposes
`sync_last_attempt`, `sync_last_response`, `sync_last_success`, `sync_last_cycle`,
`sync_in_progress` and a bounded `sync_last_error` code. Timestamps are milliseconds;
poll intervals are seconds. A response, a successful core snapshot and a changed
value are different events. An unchanged temperature is not a failed poll.

An absent zone retains its previous observations and receipt and becomes offline.
A missing field is removed from the current capability set instead of becoming
zero. Optional resources have a separate expected reading cadence. An unattempted
resource is not a successful observation and is not a demonstrated network failure.

The browser rejects older Airzone samples, reconnects its single SSE stream after
foreground/network recovery and receives a new snapshot. A delayed REST command
readback cannot restore a revoked capability or availability already changed by SSE.

## Commands in the Airzone view

The overview uses compact observation cards; opening a zone reveals its main
setpoint and everyday controls. Separate heating/cooling targets remain explicit
when the device advertises dual setpoints. Alternative targets, ventilation and
technical settings stay available one level deeper. Read-only and offline devices
show readable values instead of disabled forms. Pending command receipts remain
visible independently of the currently displayed controls or capabilities.

Controls use advertised kinds, bounds, steps, units and write permissions. Numeric
intentions coalesce for 250 ms; at most one request for a point is in flight and
the latest unsent intention wins. Reads yield to a queued command. Cancelling an
old read phase prevents its response from overwriting post-command observations.

Numeric editor formatting may shorten a float32 representation only when the
shorter decimal maps back to exactly that same float32. Display formatting never
quantizes an observation onto a command grid or changes the stored intention.

A validated API ACK records `sync_last_command_generation` before replying to the
caller. It does not publish the requested value as an equipment reading. The
dashboard obtains that barrier and waits for a matching value in a strictly newer
entity generation. A timeout is shown as unconfirmed; no ambiguous command is
automatically replayed. The targeted `target_temperature_on` action sends the
documented addressed `on + setpoint` pair and requires both readback values.

The ordinary Moli guard still applies. If a guarded request cannot be denied
after cancelling the PIN sheet, its pending status remains visible and a new
intention for that point is blocked until explicit resolution. Changing gateway
mode always has its own fresh PIN confirmation and sends no equipment command.

These intention/confirmation controls are in the dedicated Airzone view. The
existing generic Pièces/Atelier controls retain their generic presentation;
their backend commands still pass through the same native preflight and guard.

## Demonstrated regressions and test boundaries

The deterministic slow-metadata fixture showed a required HVAC observation
withheld for **4.006964 seconds** after its response. The corrected regression
requires publication before the optional read completes, with an observed upper
bound of **300 ms**. This measures an internal publication delay, not a gain in
the physical gateway's network latency.

Tests cover three consecutive external updates reaching Hub/SSE without a command
or reload, unchanged heartbeats, offline/authentication failure and recovery,
missing values, cancelled reads, stale response ordering, rapid intentions,
compound ACK validation, guarded requests and mode revocation. Individual tests,
full repository checks, browser observations and physical-house validation are
reported separately in the change review; a successful initial load is insufficient.

Manual diagnostics in Installation use a closed read-only operation enum, a
configured gateway and its known identity. Demonstration data is labelled synthetic
and never imported into live devices. The thermostat HTML is only checked for
availability/size and is never executed or embedded.

See [coverage.md](coverage.md) for the complete official API inventory and explicit
remaining cloud, broadcast and compound-write gaps. Neither passing these tests nor
adding a field to the registry establishes 100% API or physical hardware coverage.
