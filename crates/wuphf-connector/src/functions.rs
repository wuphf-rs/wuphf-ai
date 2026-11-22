//! Calls the core's remote functions: the counterpart of the Go
//! `connectors/shared/functions` client, and the `media.convert@1` contract
//! its callers and its provider (`providers/mediaconv`) share.

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::transport::{Channel, Endpoint};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::function_transport_client::FunctionTransportClient;

use crate::client::{SessionError, core_uri};
use crate::media::{MediaClient, MediaConnectError};

pub const MEDIA_CONVERT: &str = "media.convert";
pub const MEDIA_CONVERT_MAJOR: u32 = 1;

/// The args of media.convert@1; the source is the call's one media id.
#[derive(Debug, Serialize)]
pub struct MediaConvertArgs {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source_mime_type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source_file_name: String,
    pub target_mime_type: String,
}

/// The result of media.convert@1; the converted media is the one result id.
#[derive(Debug, Deserialize)]
pub struct MediaConvertResult {
    pub mime_type: String,
    pub file_name: String,
}

#[derive(Debug, Error)]
pub enum FunctionError {
    #[error("invoke {function}: encode args: {source}")]
    Encode {
        function: String,
        source: serde_json::Error,
    },
    #[error("invoke {function}: {source}")]
    Call {
        function: String,
        source: tonic::Status,
    },
    #[error("invoke {function}: decode result from provider {provider}: {source}")]
    Decode {
        function: String,
        provider: String,
        source: serde_json::Error,
    },
    #[error("{function} returned {count} media ids, want 1")]
    MediaCount { function: String, count: usize },
    #[error("download converted media: {0}")]
    Download(SessionError),
    #[error("{function} timed out after {after:?}")]
    Timeout { function: String, after: Duration },
}

/// A client of the core's FunctionTransport. Cheap to clone.
#[derive(Clone)]
pub struct FunctionsClient {
    client: FunctionTransportClient<Channel>,
    authorization: Option<MetadataValue<Ascii>>,
    caller: String,
}

impl FunctionsClient {
    /// A client for the core at `core_address`, connected on first use.
    /// `token` is the core's invoke token (none when the core requires none);
    /// `caller` labels this process in the core's logs.
    pub fn connect(
        core_address: &str,
        token: Option<&str>,
        caller: impl Into<String>,
    ) -> Result<Self, MediaConnectError> {
        let channel = Endpoint::new(core_uri(core_address)?)?.connect_lazy();
        let authorization = token
            .filter(|token| !token.is_empty())
            .map(|token| format!("Bearer {token}").parse())
            .transpose()?;
        Ok(Self {
            client: FunctionTransportClient::new(channel),
            authorization,
            caller: caller.into(),
        })
    }

    /// Call `name@major` with JSON `args` and `media_ids`, decoding the JSON
    /// result; returns it with the result's media ids. `deadline` becomes the
    /// call's gRPC timeout, which the core honours while it waits for a
    /// provider.
    pub async fn invoke<A: Serialize, R: DeserializeOwned>(
        &self,
        name: &str,
        major: u32,
        args: &A,
        media_ids: Vec<String>,
        deadline: Option<Duration>,
    ) -> Result<(R, Vec<String>), FunctionError> {
        let function = format!("{name}@{major}");
        let args = serde_json::to_vec(args).map_err(|source| FunctionError::Encode {
            function: function.clone(),
            source,
        })?;
        let mut request = tonic::Request::new(pb::InvokeRequest {
            function: Some(pb::FunctionRef {
                name: name.to_owned(),
                major,
            }),
            args,
            media_ids,
            caller: self.caller.clone(),
        });
        if let Some(authorization) = &self.authorization {
            request
                .metadata_mut()
                .insert("authorization", authorization.clone());
        }
        if let Some(deadline) = deadline {
            request.set_timeout(deadline);
        }
        let response = self
            .client
            .clone()
            .invoke(request)
            .await
            .map_err(|source| FunctionError::Call {
                function: function.clone(),
                source,
            })?
            .into_inner();
        let result =
            serde_json::from_slice(&response.result).map_err(|source| FunctionError::Decode {
                function,
                provider: response.provider_id,
                source,
            })?;
        Ok((result, response.media_ids))
    }
}

/// Converts stored media through media.convert@1 and downloads the result.
/// `timeout` is read on every call (Go's `TimeoutFunc`), so a timeout that is
/// a runtime setting applies to the next conversion; it bounds the whole call,
/// waiting for a provider and the download included.
#[derive(Clone)]
pub struct MediaConverter {
    pub functions: FunctionsClient,
    pub media: MediaClient,
    pub timeout: Arc<dyn Fn() -> Duration + Send + Sync>,
}

impl MediaConverter {
    pub async fn convert(
        &self,
        media_id: &str,
        source_mime: &str,
        source_name: &str,
        target_mime: &str,
    ) -> Result<Vec<u8>, FunctionError> {
        let timeout = (self.timeout)();
        let call = async {
            let args = MediaConvertArgs {
                source_mime_type: source_mime.to_owned(),
                source_file_name: source_name.to_owned(),
                target_mime_type: target_mime.to_owned(),
            };
            let (_, ids): (MediaConvertResult, _) = self
                .functions
                .invoke(
                    MEDIA_CONVERT,
                    MEDIA_CONVERT_MAJOR,
                    &args,
                    vec![media_id.to_owned()],
                    Some(timeout),
                )
                .await?;
            let [id] = ids.as_slice() else {
                return Err(FunctionError::MediaCount {
                    function: format!("{MEDIA_CONVERT}@{MEDIA_CONVERT_MAJOR}"),
                    count: ids.len(),
                });
            };
            self.media
                .download(id)
                .await
                .map_err(FunctionError::Download)
        };
        let timed_out = || FunctionError::Timeout {
            function: format!("{MEDIA_CONVERT}@{MEDIA_CONVERT_MAJOR}"),
            after: timeout,
        };
        match tokio::time::timeout(timeout, call).await {
            // The gRPC deadline can fire first: the transport then cancels the
            // call, which is the same timeout.
            Ok(Err(FunctionError::Call { source, .. }))
                if matches!(
                    source.code(),
                    tonic::Code::Cancelled | tonic::Code::DeadlineExceeded
                ) =>
            {
                Err(timed_out())
            }
            Ok(result) => result,
            Err(_) => Err(timed_out()),
        }
    }
}
