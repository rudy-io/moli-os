use super::{
    CloudClient, CollectionKind, Endpoint, Error, Query, SessionSink, Transport, auth_status,
    inventory,
};
use crate::cloud::execution::{self, Ticket};

impl<T: Transport> CloudClient<T> {
    pub(crate) async fn execute_command(
        &mut self,
        command: moli_runtime::CommandRequest,
        ticket: &Ticket,
        sink: &mut impl SessionSink,
    ) -> Result<(), Error> {
        let result = self
            .execute_scenario(ticket, command.deadline, || !command.abandoned(), sink)
            .await;
        command.reply(result.map_err(|error| error.to_string()));
        result
    }
    pub(crate) fn execution_read_only(&mut self, read_only: bool) {
        self.execution_read_only = read_only;
    }
    pub(crate) async fn execute_scenario(
        &mut self,
        ticket: &Ticket,
        deadline: std::time::Instant,
        waiting: impl Fn() -> bool,
        sink: &mut impl SessionSink,
    ) -> Result<(), Error> {
        if self.execution_read_only {
            return Err(Error::Forbidden);
        }
        let mut sent = false;
        let waiting = || waiting() && std::time::Instant::now() < deadline;
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let result = tokio::time::timeout(
            remaining,
            self.scenario_inner(ticket, &waiting, sink, &mut sent),
        )
        .await
        .unwrap_or(Err(if sent {
            Error::ExecutionUncertain
        } else {
            Error::Deadline
        }));
        if let Err(error) = result {
            self.verified = false;
            self.failure = Some(error);
        }
        result
    }
    async fn scenario_inner(
        &mut self,
        ticket: &Ticket,
        waiting: &impl Fn() -> bool,
        sink: &mut impl SessionSink,
        sent: &mut bool,
    ) -> Result<(), Error> {
        if !waiting() {
            return Err(Error::Deadline);
        }
        if self.identity()? != ticket.identity || !execution::valid_oid(&ticket.oid) {
            return Err(Error::ScopeMismatch);
        }
        self.verify_scope(sink).await?;
        if !waiting() {
            return Err(Error::Deadline);
        }
        if self.identity()? != ticket.identity {
            return Err(Error::ScopeMismatch);
        }
        let current = self
            .authorized(
                Endpoint::Collection(CollectionKind::Scenarios),
                Query::None,
                sink,
            )
            .await?;
        auth_status(&current, self.backoff)?;
        let definitions = inventory::parse_collection(CollectionKind::Scenarios, &current.body)
            .map_err(|_| Error::InvalidResponse)?;
        let matches = definitions.definitions.iter().any(|definition| {
            definition.id.as_ref() == Some(&ticket.oid)
                && definition
                    .raw
                    .as_value()
                    .get("oid")
                    .and_then(serde_json::Value::as_str)
                    == Some(ticket.oid.as_str())
                && execution::fingerprint(definition).ok().as_ref() == Some(&ticket.fingerprint)
        });
        if !matches || self.identity()? != ticket.identity {
            return Err(Error::ScopeMismatch);
        }
        if !waiting() {
            return Err(Error::Deadline);
        }
        self.check_rate()?;
        // No awaited work before the single POST. Unlike reads, this path never retries 401.
        *sent = true;
        let response = self
            .authenticated_send(Endpoint::Scenario, Query::Scenario(ticket.oid.clone()))
            .await
            .map_err(|_| Error::ExecutionUncertain)?;
        auth_status(&response, self.backoff)?;
        let ack: serde_json::Value =
            serde_json::from_slice(&response.body).map_err(|_| Error::ExecutionUncertain)?;
        if !ack
            .get("execId")
            .and_then(serde_json::Value::as_str)
            .is_some_and(execution::valid_oid)
        {
            return Err(Error::ExecutionUncertain);
        }
        Ok(())
    }
}
