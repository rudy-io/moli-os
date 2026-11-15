//! Internal: TLV context-tag constants for certificate top-level
//! fields and distinguished-name attributes. Spec §6.5.

#![allow(dead_code)] // Some constants land in later phases (M2.2, M2.3).

// --- Top-level certificate field tags (spec §6.5) ---
pub(crate) const CERT_SERIAL_NUMBER: u8 = 1;
pub(crate) const CERT_SIG_ALGORITHM: u8 = 2;
pub(crate) const CERT_ISSUER: u8 = 3;
pub(crate) const CERT_NOT_BEFORE: u8 = 4;
pub(crate) const CERT_NOT_AFTER: u8 = 5;
pub(crate) const CERT_SUBJECT: u8 = 6;
pub(crate) const CERT_PUBKEY_ALGORITHM: u8 = 7;
pub(crate) const CERT_EC_CURVE: u8 = 8;
pub(crate) const CERT_EC_PUBLIC_KEY: u8 = 9;
pub(crate) const CERT_EXTENSIONS: u8 = 10;
pub(crate) const CERT_SIGNATURE: u8 = 11;

// --- DN attribute tags (spec §6.5.6 Table 71) ---
pub(crate) const DN_COMMON_NAME: u8 = 1;
pub(crate) const DN_SURNAME: u8 = 2;
pub(crate) const DN_SERIAL_NUMBER: u8 = 3;
pub(crate) const DN_COUNTRY_NAME: u8 = 4;
pub(crate) const DN_LOCALITY_NAME: u8 = 5;
pub(crate) const DN_STATE_OR_PROVINCE: u8 = 6;
pub(crate) const DN_ORGANIZATION_NAME: u8 = 7;
pub(crate) const DN_ORG_UNIT_NAME: u8 = 8;
pub(crate) const DN_TITLE: u8 = 9;
pub(crate) const DN_NAME: u8 = 10;
pub(crate) const DN_GIVEN_NAME: u8 = 11;
pub(crate) const DN_INITIALS: u8 = 12;
pub(crate) const DN_GENERATION_QUALIFIER: u8 = 13;
pub(crate) const DN_DN_QUALIFIER: u8 = 14;
pub(crate) const DN_PSEUDONYM: u8 = 15;
pub(crate) const DN_DOMAIN_COMPONENT: u8 = 16;
pub(crate) const DN_MATTER_NODE_ID: u8 = 17;
// Tag 18 is matter-firmware-signing-id — treated as Other in M2.1.
pub(crate) const DN_MATTER_ICAC_ID: u8 = 19;
pub(crate) const DN_MATTER_RCAC_ID: u8 = 20;
pub(crate) const DN_MATTER_FABRIC_ID: u8 = 21;
pub(crate) const DN_MATTER_NOC_CAT: u8 = 22;
// Tag 23 is matter-vvs-id (chip `MatterVidVerificationSignerId`, matter.js
// `vvsId`, OID 1.3.6.1.4.1.37244.1.7); 24-26 are assigned by neither
// reference. None is declared: no `DnAttribute` variant maps to them, so the
// decoder keeps them untyped (`DnAttribute::Other`).

// --- Algorithm identifiers (spec §6.5.2) ---
pub(crate) const SIG_ALGORITHM_ECDSA_SHA256: u8 = 1;
pub(crate) const PUBKEY_ALGORITHM_EC_PUBLIC_KEY: u8 = 1;
pub(crate) const EC_CURVE_PRIME256V1: u8 = 1;

// --- Extension tags (spec §6.5.4) ---
pub(crate) const EXT_BASIC_CONSTRAINTS: u8 = 1;
pub(crate) const EXT_KEY_USAGE: u8 = 2;
pub(crate) const EXT_EXTENDED_KEY_USAGE: u8 = 3;
pub(crate) const EXT_SUBJECT_KEY_IDENTIFIER: u8 = 4;
pub(crate) const EXT_AUTHORITY_KEY_IDENTIFIER: u8 = 5;

// --- BasicConstraints sub-tags (spec §6.5.4.1) ---
pub(crate) const BC_IS_CA: u8 = 1;
pub(crate) const BC_PATH_LEN_CONSTRAINT: u8 = 2;
