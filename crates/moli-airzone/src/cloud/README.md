# Airzone Cloud Web API reader

This optional module is separate from the native Local API driver. It implements
the documented normal-account login, token rotation, installation inventory and
read-only device, gateway, location and profile requests. It does not add device
commands, cloud schedules or a WebSocket subscription.

## Official contract

Reviewed on 2026-11-04:

- [Web API 1.0](https://developers.airzonecloud.com/docs/web-api/), specification
  `https://developers.airzonecloud.com/downloads/webapi.openapi.yml`.
- Fixed REST origin `https://m.airzonecloud.com/api/v1` with public certificate
  validation. The caller cannot change host, protocol, port, path or HTTP method.
- `POST /auth/login` takes `email` and `password`. The documentation states a
  48-hour access JWT and a one-week, one-use refresh token. Rotation uses
  `GET /auth/refreshToken/{refreshToken}` and invalidates the old pair.
- `pendingMigration=true` prohibits access to installation data.
- Every device/gateway status or configuration call requires
  `installation_id`. Configuration additionally requires `type=user|advanced|all`;
  advanced/all and gateway `devices=1` require admin permission. A forbidden
  response stays visible instead of being retried or silently downgraded.
- Installation pagination uses `items` (1–10) and non-negative `page`. This reader
  starts at page 0, checks the reported total, duplicates and every subsequent
  page. A changed total, missing page or limit produces an explicit incomplete
  inventory error; no partial list is presented as complete.

The published Web API has **no schedules endpoint**, despite containing schedule
schemas. The six schedule operations belong to
[Open API 1.2](https://developers.airzonecloud.com/docs/open-api/) at
`https://m.airzonecloud.com/api/v2/open`: GET/POST/DELETE schedules,
PATCH schedules/activate, and PATCH/DELETE an individual schedule. That API
requires Airzone's manual OAuth application approval and user consent. A normal
account is sufficient for the documented Web API login, not proof of an approved
Open API application. No undocumented schedule route is inferred.

## Application contract

Use `CloudClient::with_vault(hub)` in the application. It restores the encrypted
`core/airzone_cloud_session` bundle without network activity. Login is refused
before contacting Airzone when the Hub has no secret store. `new()` is an
explicitly memory-only reader, useful for isolated consumers/tests.

The password is used only during login and is never persisted. Access and refresh
tokens are kept in the existing encrypted Hub vault. Neither credentials nor
raw HTTP/backend errors have a printable Debug/error path. The access JWT's
expiry is inspected only as a scheduling hint; this does not claim local
signature verification. `AuthStatus::Connected.expires_at` is Unix **seconds**.

Refresh is single-flight: the previous stored token is removed before the
request, and the rotated pair must be persisted before any dependent read.
Timeout, cancellation, invalid response or persistence failure leaves an explicit
authentication-required state. There is no replay of a possibly consumed refresh
token. A read rejected with 401 may retry once after successful rotation; 403,
429 and other errors do not trigger retry loops.

`disconnect()` forgets only this reader's local/vault session. It does not invoke
the provider's `GET /user/logout`, which invalidates the access/refresh pair used
by that request. Provider-side revocation remains an explicit unimplemented
operation; forgetting local credentials does not claim to revoke that pair.

This module's stable `Error` values are internal. The application maps them to
French and English UI strings. The module does not publish account data or
device documents to logs. Device documents preserve null and missing fields,
bound nested values, reject credential-shaped fields and expose fallible common
accessors; their presence grants no write capability.

## WebSocket protocol findings — not implemented

The [official WebSocket documentation](https://developers.airzonecloud.com/docs/websocket-api/)
publishes production WSS at `m.airzonecloud.com/api/v1/websockets/user` and
`m.airzonecloud.com/api/v1/websockets/installation?installationId=…`.
It is a companion to Web API authentication, not Open API OAuth registration.

An installation subscription first sends `DEVICE_STATE` snapshots followed by
`DEVICE_STATE_END`, then incremental `DEVICES_UPDATES` and `WEBSERVER_UPDATES`
events. User/account and installation events use separate subscriptions. Each
installation requires its own connection.

The server sends correlated authentication challenges. The client must reply
with `{corrId, body:{jwt}}` within five seconds; missing/invalid/expired credentials
cause close code 4001. A production event reader still needs bounded snapshots,
delta merge, reconnect/resubscription, identity checks, snapshot completion and
token-renewal coordination. The documentation's example disables TLS checking;
that example must not be copied into Moli. No event support is claimed here.

All module tests use fictional accounts, devices, tokens and an injected
transport. They do not contact Airzone or operate physical equipment.
