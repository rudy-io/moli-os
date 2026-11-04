//! Optional cloud consultation. Local driver supervision is independent.
use super::Manager;
use moli_airzone::cloud::{self, ConfigSection};
use moli_api::airzone_cloud::{CloudError, LoginRequest, ReadRequest};
use serde::Serialize;
use serde_json::{Value, json};

fn error(error: cloud::Error) -> CloudError {
    CloudError::new(match error {
        cloud::Error::InvalidInput => "invalid_input",
        cloud::Error::AuthenticationRequired => "authentication_required",
        cloud::Error::MigrationPending => "migration_pending",
        cloud::Error::Forbidden => "forbidden",
        cloud::Error::RateLimited => "rate_limited",
        cloud::Error::Unavailable => "unavailable",
        cloud::Error::Transport => "transport",
        cloud::Error::InvalidResponse => "invalid_response",
        cloud::Error::IncompleteInventory => "incomplete_inventory",
        cloud::Error::Storage => "storage",
    })
}

fn data(value: impl Serialize) -> Result<Value, CloudError> {
    serde_json::to_value(value).map_err(|_| CloudError::new("invalid_response"))
}

impl Manager {
    pub(super) async fn cloud_operation<T>(
        &self,
        future: impl std::future::Future<Output = Result<T, CloudError>>,
    ) -> Result<T, CloudError> {
        tokio::select! {
            biased;
            ()=self.0.cloud_cancel.cancelled()=>Err(CloudError::new("unavailable")),
            result=future=>result,
        }
    }
    fn cloud_available(&self) -> Result<(), CloudError> {
        self.available().map_err(|_| CloudError::new("unavailable"))
    }

    pub(super) async fn cloud_report(&self) -> Result<Value, CloudError> {
        self.cloud_available()?;
        let mut report = data(
            self.cloud_operation(async { Ok(self.0.cloud.auth_status().await) })
                .await?,
        )?;
        report["read_only"] = json!(true);
        report["persistence_available"] = json!(self.0.hub.has_secret_store());
        Ok(report)
    }

    pub(super) async fn connect_cloud(&self, request: LoginRequest) -> Result<Value, CloudError> {
        self.cloud_available()?;
        let _permit = self
            .0
            .cloud_operations
            .try_acquire()
            .map_err(|_| CloudError::new("busy"))?;
        self.cloud_operation(async {
            self.0
                .cloud
                .login(&request.email, &request.password)
                .await
                .map_err(error)
        })
        .await?;
        self.cloud_report().await
    }

    pub(super) async fn disconnect_cloud(&self) -> Result<Value, CloudError> {
        self.cloud_available()?;
        let _permit = self
            .0
            .cloud_operations
            .try_acquire()
            .map_err(|_| CloudError::new("busy"))?;
        self.cloud_operation(async { self.0.cloud.disconnect().await.map_err(error) })
            .await?;
        self.cloud_report().await
    }

    pub(super) async fn read_cloud(&self, request: ReadRequest) -> Result<Value, CloudError> {
        self.cloud_available()?;
        let _permit = self
            .0
            .cloud_operations
            .try_acquire()
            .map_err(|_| CloudError::new("busy"))?;
        let client = &self.0.cloud;
        let value = self
            .cloud_operation(async {
                Ok(match request {
                    ReadRequest::Installations {} => {
                        data(client.installations().await.map_err(error)?)?
                    }
                    ReadRequest::Installation { installation_id } => {
                        data(client.installation(&installation_id).await.map_err(error)?)?
                    }
                    ReadRequest::DeviceStatus {
                        installation_id,
                        device_id,
                    } => data(
                        client
                            .device_status(&installation_id, &device_id)
                            .await
                            .map_err(error)?,
                    )?,
                    ReadRequest::DeviceConfig {
                        installation_id,
                        device_id,
                        section,
                    } => {
                        let section = match section {
                            moli_api::airzone_cloud::ConfigSection::All => ConfigSection::All,
                            moli_api::airzone_cloud::ConfigSection::User => ConfigSection::User,
                            moli_api::airzone_cloud::ConfigSection::Advanced => {
                                ConfigSection::Advanced
                            }
                        };
                        data(
                            client
                                .device_config(&installation_id, &device_id, section)
                                .await
                                .map_err(error)?,
                        )?
                    }
                    ReadRequest::GatewayStatus {
                        installation_id,
                        gateway_id,
                        include_devices,
                    } => data(
                        client
                            .gateway_status(&installation_id, &gateway_id, include_devices)
                            .await
                            .map_err(error)?,
                    )?,
                    ReadRequest::Location { location_id } => {
                        data(client.location(&location_id).await.map_err(error)?)?
                    }
                    ReadRequest::Profile {} => data(client.profile().await.map_err(error)?)?,
                })
            })
            .await?;
        Ok(json!({"read_only":true,"received_at":moli_core::now_ms(),"data":value}))
    }
}
