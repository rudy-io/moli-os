use super::{
    Catalog, Controller, DriverCtx, Error, Job, JobKind, JobTask, PendingRequest, RequestEnvelope,
    Work, inventory, now_ms,
};
use crate::cloud::execution::{self, Policy};
use moli_runtime::CommandRequest;

impl Controller {
    pub(crate) fn dispatch(&mut self, ctx: &DriverCtx, command: CommandRequest) {
        let prepared = (|| {
            if self.scenario_mode.read_only()
                || !self.live
                || self.busy
                || self.job.is_some()
                || !self.gate.open()
                || ctx.secret(super::super::PENDING_KEY).is_some()
                || command.abandoned()
            {
                return Err(Error::Forbidden);
            }
            let (catalog, revision) = Self::execution_catalog(ctx)?;
            if !execution::fresh(&catalog, self.poll.as_secs()) {
                return Err(Error::ScopeMismatch);
            }
            let policy = ctx
                .secret(execution::POLICY_KEY)
                .and_then(|s| Policy::decode(s.as_bytes()).ok())
                .unwrap_or_default();
            execution::resolve(
                &catalog,
                &policy,
                &revision,
                &command.device.id,
                &command.key,
                &command.value,
            )
        })();
        match prepared {
            Ok(ticket) => {
                let Some(actor) = &self.actor else {
                    command.reply(Err(Error::Network.to_string()));
                    return;
                };
                match actor
                    .commands
                    .try_send(Work::Execute(command, Box::new(ticket)))
                {
                    Ok(()) => self.busy = true,
                    Err(error) => {
                        if let Work::Execute(command, _) = error.into_inner() {
                            command.reply(Err(Error::Network.to_string()));
                        }
                    }
                }
            }
            Err(error) => command.reply(Err(error.to_string())),
        }
    }

    fn execution_catalog(ctx: &DriverCtx) -> Result<(Catalog, String), Error> {
        let binding = ctx
            .secret(super::super::SESSION_KEY)
            .and_then(|s| {
                super::super::read_session_binding(s.as_bytes(), ctx.instance().as_str()).ok()
            })
            .ok_or(Error::ScopeMismatch)?;
        let bytes = ctx
            .secret(inventory::INVENTORY_KEY)
            .ok_or(Error::ScopeMismatch)?;
        let catalog =
            inventory::decode_catalog(bytes.as_bytes()).map_err(|_| Error::ScopeMismatch)?;
        if catalog.instance != ctx.instance().as_str()
            || catalog.connection_generation != binding.generation
            || Some(&catalog.scope) != binding.scope.as_ref()
        {
            return Err(Error::ScopeMismatch);
        }
        Ok((catalog, execution::revision(bytes.as_bytes())))
    }

    pub(super) fn permission(&mut self, ctx: &DriverCtx, envelope: RequestEnvelope) {
        let PendingRequest::ScenarioPermission {
            generation,
            revision,
            scenario_id,
            enabled,
        } = envelope.request
        else {
            return;
        };
        let mut state = self.state.clone();
        state.request_id = Some(envelope.request_id);
        let stored = Self::execution_catalog(ctx).ok();
        let catalog = stored.as_ref().map(|(catalog, _)| catalog.clone());
        let mut policy = ctx
            .secret(execution::POLICY_KEY)
            .and_then(|s| Policy::decode(s.as_bytes()).ok())
            .unwrap_or_default();
        let result = (|| {
            let catalog = catalog.as_ref().ok_or(Error::ScopeMismatch)?;
            if catalog.connection_generation != generation
                || (enabled && stored.as_ref().map(|(_, hash)| hash) != Some(&revision))
            {
                return Err(Error::ScopeMismatch);
            }
            if enabled
                && (now_ms() < envelope.submitted_at_ms
                    || now_ms().saturating_sub(envelope.submitted_at_ms) > 60_000)
            {
                return Err(Error::ScopeMismatch);
            }
            if enabled
                && (self.scenario_mode.read_only()
                    || !self.live
                    || !execution::fresh(catalog, self.poll.as_secs()))
            {
                return Err(Error::Forbidden);
            }
            policy.change(catalog, &scenario_id, enabled)?;
            policy.encode()
        })();
        let mut values = vec![];
        match result {
            Ok(encoded) => {
                values.push((execution::POLICY_KEY.into(), encoded));
                state.failure = None;
            }
            Err(error) => state.failure = Some(error),
        }
        let Ok(encoded) = super::super::encode_state(&state)
            .and_then(|b| String::from_utf8(b).map_err(|_| Error::InvalidResponse))
        else {
            return;
        };
        values.push((super::super::STATE_KEY.into(), encoded));
        let observed = self.live;
        self.live = false;
        execution::publish(
            ctx,
            catalog.as_ref(),
            self.scenario_mode.read_only(),
            false,
            self.poll.as_secs(),
        );
        self.job = Some(Job {
            task: JobTask::Write(ctx.store_secrets_job(values)),
            kind: JobKind::Cycle {
                epoch: self.epoch,
                state: Box::new(state),
                catalog: catalog.map(Box::new),
                observed,
            },
        });
    }
}
