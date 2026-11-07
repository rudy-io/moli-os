//! Cached values have separate live and durable representations.
use super::{ConnectionState, Error};
use crate::inventory::{self, Catalog};

pub(super) fn snapshot(
    state: &ConnectionState,
    catalog: Option<&Catalog>,
) -> Result<Vec<(String, String, String)>, Error> {
    let mut values = vec![];
    if let Some(catalog) = catalog {
        let value = String::from_utf8(
            inventory::encode_catalog(catalog).map_err(|_| Error::InvalidResponse)?,
        )
        .map_err(|_| Error::InvalidResponse)?;
        let mut durable = catalog.clone();
        for collection in durable.collections.values_mut() {
            // Preserve validated data/status while never restoring a fresh read.
            collection.last_attempt_ms = collection.last_attempt_ms.map(|_| 0);
            collection.last_observation_ms = collection.last_observation_ms.map(|_| 0);
        }
        let durable = String::from_utf8(
            inventory::encode_catalog(&durable).map_err(|_| Error::InvalidResponse)?,
        )
        .map_err(|_| Error::InvalidResponse)?;
        values.push((inventory::INVENTORY_KEY.into(), value, durable));
    }
    let value =
        String::from_utf8(super::encode_state(state)?).map_err(|_| Error::InvalidResponse)?;
    let mut durable = state.clone();
    durable.last_observation_ms = None;
    if durable.retry_after_secs.is_none() {
        durable.last_attempt_ms = None;
    }
    let durable =
        String::from_utf8(super::encode_state(&durable)?).map_err(|_| Error::InvalidResponse)?;
    values.push((super::STATE_KEY.into(), value, durable));
    Ok(values)
}

pub(super) fn cached(ctx: &moli_runtime::DriverCtx, name: &str) -> Option<String> {
    ctx.private_state()?.get(name).or_else(|| ctx.secret(name))
}

pub(super) fn has_legacy(ctx: &moli_runtime::DriverCtx) -> bool {
    [
        super::STATE_KEY,
        inventory::INVENTORY_KEY,
        super::PENDING_KEY,
        super::execution::POLICY_KEY,
    ]
    .iter()
    .any(|name| ctx.secret(name).is_some())
}

pub(super) fn legacy(
    ctx: &moli_runtime::DriverCtx,
) -> Result<Vec<(String, String, String)>, Error> {
    let state = ctx
        .secret(super::STATE_KEY)
        .map(|value| super::decode_state(value.as_bytes()))
        .transpose()?
        .unwrap_or_default();
    let catalog = ctx
        .secret(inventory::INVENTORY_KEY)
        .map(|value| {
            inventory::decode_catalog(value.as_bytes()).map_err(|_| Error::InvalidResponse)
        })
        .transpose()?;
    let mut entries = snapshot(&state, catalog.as_ref())?;
    if let Some(pending) = ctx.secret(super::PENDING_KEY) {
        if pending.len() > super::MAX_PENDING_BYTES {
            return Err(Error::InvalidInput);
        }
        entries.push((super::PENDING_KEY.into(), pending.clone(), pending));
    }
    if let Some(policy) = ctx.secret(super::execution::POLICY_KEY) {
        let policy = super::execution::Policy::decode(policy.as_bytes())?.encode()?;
        entries.push((super::execution::POLICY_KEY.into(), policy.clone(), policy));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::{CatalogIdentity, CollectionKind, Region, Scope};

    fn catalog(at: i64) -> Catalog {
        let identity = CatalogIdentity {
            instance: "synthetic".into(),
            connection_generation: "generation".into(),
            scope: Scope {
                site_oid: "setup".into(),
                gateway_id: "anchor".into(),
                region: Region::Emea,
            },
        };
        let mut catalog = Catalog::new(identity.clone()).unwrap();
        catalog
            .apply_collection(
                &identity,
                CollectionKind::Scenarios,
                at,
                Ok(inventory::parse_collection(CollectionKind::Scenarios, b"[]").unwrap()),
            )
            .unwrap();
        catalog
    }

    #[test]
    fn poll_timestamps_update_live_values_without_changing_durable_snapshot() {
        let state = ConnectionState {
            last_attempt_ms: Some(1000),
            last_observation_ms: Some(1000),
            ..Default::default()
        };
        let first = snapshot(&state, Some(&catalog(1000))).unwrap();
        let next_state = ConnectionState {
            last_attempt_ms: Some(2000),
            last_observation_ms: Some(2000),
            ..state
        };
        let next = snapshot(&next_state, Some(&catalog(2000))).unwrap();
        assert_ne!(first[0].1, next[0].1);
        assert_ne!(first[1].1, next[1].1);
        assert_eq!(first[0].2, next[0].2);
        assert_eq!(first[1].2, next[1].2);
        let persisted = inventory::decode_catalog(first[0].2.as_bytes()).unwrap();
        assert_eq!(
            persisted.collections[&CollectionKind::Scenarios].last_observation_ms,
            Some(0)
        );
        assert!(
            persisted.collections[&CollectionKind::Scenarios]
                .data
                .is_some()
        );
        let state = super::super::decode_state(first[1].2.as_bytes()).unwrap();
        assert_eq!(state.last_observation_ms, None);
        assert_eq!(state.last_attempt_ms, None);
    }

    #[test]
    fn quota_anchor_and_scope_changes_remain_durable() {
        let state = ConnectionState {
            retry_after_secs: Some(60),
            last_attempt_ms: Some(1000),
            ..Default::default()
        };
        let first = snapshot(&state, Some(&catalog(1000))).unwrap();
        let mut changed = catalog(2000);
        changed.connection_generation = "replacement".into();
        let next = snapshot(&state, Some(&changed)).unwrap();
        assert_ne!(first[0].2, next[0].2);
        let persisted = super::super::decode_state(first[1].2.as_bytes()).unwrap();
        assert_eq!(persisted.retry_after_secs, Some(60));
        assert_eq!(persisted.last_attempt_ms, Some(1000));
    }
}
