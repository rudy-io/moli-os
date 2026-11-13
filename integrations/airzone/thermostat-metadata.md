# Thermostat metadata: battery and radio

The Local API [1.78 schema](https://developers.airzonecloud.com/downloads/local-api/1.78/lapi_openapi.yaml)
provides `thermos_type`, `thermos_firmware`, `thermos_radio`, `battery` and
`coverage` on zone readings. They are optional, model-dependent observations.

- `thermos_radio = 0` means wired; `1` means radio. A missing value does not identify either connection.
- `battery` and `coverage` are integer percentages, including zero. The documented eligibility is Flexa >= 3.2.0, Acuazone >= 4.2.0 or VAF >= 7.2.0, with a radio thermostat. System firmware alone does not establish thermostat eligibility.
- A wired thermostat does not require battery or radio placeholders. Moli shows its reported model and firmware when available.
- An omitted, null, invalid or unsupported percentage is never converted to zero. Only an explicit low-battery alarm creates that warning; Moli adds no percentage threshold.
- Missing local fields are explained as information not supplied by the API, not as proof of a broken thermostat or a low battery. An offline or old reading remains labelled last known.

## Investigating a missing value

1. Compare the current zone response from the bounded `POST /hvac` read with its advertised points in Moli. Use a zone-specific read to confirm an omission from the broadcast response; never use PUT for diagnosis.
2. Compare the thermostat's display or official Airzone application. Check the connection type and reported firmware, without inferring radio capability from a room name.
3. The optional [Web API](https://developers.airzonecloud.com/downloads/webapi.openapi.yml) has separate cloud status/configuration documents and optional `radio_batt_percent` / `radio_coverage_percent` fields. These do not establish that a particular account can read them. An HTTP 402 configuration response is an access restriction, not a battery value or evidence that purchasing a plan would expose it.
4. Keep cloud data separate from local observations. A cloud record must not overwrite a missing local value based on a display name or ambiguous system/zone identity.

The thermostat card only renders existing typed Moli observations. It performs no
network request or equipment command. The gateway diagnostics link leads to the
existing explicit read-only verification workflow.
