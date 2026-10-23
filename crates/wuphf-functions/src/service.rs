//! The two gRPC faces of the broker. [`GrpcFunctionService`] serves callers:
//! it validates a call, queues it with the hub and waits for its outcome within
//! the call's deadline. [`GrpcProviderService`] serves providers: it
//! authenticates a session, registers its functions and relays results to the
//! hub until the provider's stream ends.

use crate::config::{FunctionKey, Functions};
use crate::hub::{
    AbandonReport, Call, CallFailure, CallId, CallOutcome, FunctionHubHandle, InvokeRejection,
    RegisterRejection, is_json,
};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status};
use tracing::{info, warn};
use uuid::Uuid;
use wuphf_proto::connector as pb;
use wuphf_proto::connector::function_transport_server::FunctionTransport;
use wuphf_proto::connector::provider_transport_server::ProviderTransport;

/// Room in a provider's output channel beyond its calls and cancels.
const OUTPUT_SLACK: usize = 16;

pub struct GrpcFunctionService {
    hub: FunctionHubHandle,
    functions: Arc<Functions>,
}

impl GrpcFunctionService {
    pub fn new(hub: FunctionHubHandle, functions: Arc<Functions>) -> Self {
        Self { hub, functions }
    }
}

/// Abandons its call when dropped while armed. tonic drops the `Invoke` handler
/// when the caller cancels or its deadline passes, and this turns that drop
/// into a `CancelCall` at the provider.
struct CallGuard {
    id: Option<CallId>,
    hub: FunctionHubHandle,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.hub.abandon(id);
        }
    }
}

#[tonic::async_trait]
impl FunctionTransport for GrpcFunctionService {
    async fn invoke(
        &self,
        request: Request<pb::InvokeRequest>,
    ) -> Result<Response<pb::InvokeResponse>, Status> {
        let started = Instant::now();
        let requested = grpc_timeout(request.metadata());
        let request = request.into_inner();
        let key = FunctionKey::from_proto(request.function).map_err(Status::invalid_argument)?;
        let cap = self.functions.max_body_bytes(&key.name);
        if request.args.len() > cap {
            return Err(Status::invalid_argument(format!(
                "args are {} bytes; the cap for {key} is {cap}; pass bulky data as media_ids",
                request.args.len()
            )));
        }
        if !is_json(&request.args) {
            return Err(Status::invalid_argument("args must be UTF-8 JSON"));
        }

        let id = Uuid::new_v4().to_string();
        let caller = request.caller;
        let (deadline, outcome) = self
            .hub
            .invoke(Call {
                id: id.clone(),
                key: key.clone(),
                args: request.args,
                media_ids: request.media_ids,
                caller: caller.clone(),
                requested,
            })
            .await
            .map_err(|rejection| match rejection {
                InvokeRejection::NotServable => {
                    Status::not_found(format!("no configured provider may serve {key}"))
                }
                InvokeRejection::QueueFull { limit } => Status::resource_exhausted(format!(
                    "{key} already has {limit} calls waiting for a provider"
                )),
                InvokeRejection::Stopped => Status::internal("function hub stopped"),
            })?;
        let mut guard = CallGuard {
            id: Some(id),
            hub: self.hub.clone(),
        };

        let waited = tokio::time::timeout(deadline, outcome).await;
        let id = guard.id.take().expect("armed above");
        let (result, provider, queued_for) = match waited {
            Ok(Ok(CallOutcome::Completed {
                result,
                media_ids,
                provider_id,
                queued_for,
            })) => (
                Ok(pb::InvokeResponse {
                    result,
                    media_ids,
                    provider_id: provider_id.clone(),
                }),
                provider_id,
                Some(queued_for),
            ),
            Ok(Ok(CallOutcome::Failed(CallFailure::Provider {
                code,
                message,
                provider_id,
            }))) => {
                let message = format!("{key} failed on provider {provider_id}: {message}");
                let status = match code {
                    pb::CallErrorCode::InvalidArgument => Status::invalid_argument(message),
                    pb::CallErrorCode::Unsupported => Status::unimplemented(message),
                    pb::CallErrorCode::Failed | pb::CallErrorCode::Unspecified => {
                        Status::internal(message)
                    }
                };
                (Err(status), provider_id, None)
            }
            Ok(Ok(CallOutcome::Failed(CallFailure::ProviderLost { provider_id }))) => (
                Err(Status::unavailable(format!(
                    "provider {provider_id} disconnected during the call"
                ))),
                provider_id,
                None,
            ),
            Ok(Err(_)) => (
                Err(Status::internal("function hub stopped")),
                String::new(),
                None,
            ),
            Err(_) => {
                let report = self.hub.abandon_and_report(id).await;
                let (message, provider) = match report {
                    AbandonReport::Queued {
                        provider_connected: false,
                    }
                    | AbandonReport::Unknown => (
                        format!("no provider for {key} connected within {deadline:?}"),
                        String::new(),
                    ),
                    AbandonReport::Queued {
                        provider_connected: true,
                    } => (
                        format!("{key} stayed queued behind busy providers for {deadline:?}"),
                        String::new(),
                    ),
                    AbandonReport::InFlight { provider_id } => (
                        format!("provider {provider_id} did not answer {key} within {deadline:?}"),
                        provider_id,
                    ),
                };
                (Err(Status::deadline_exceeded(message)), provider, None)
            }
        };
        info!(
            function = %key,
            caller = %caller,
            provider = %provider,
            outcome = %result.as_ref().map_or_else(|status| format!("{:?}", status.code()), |_| "Ok".into()),
            queued_ms = queued_for.map(|queued| queued.as_millis() as u64),
            total_ms = started.elapsed().as_millis() as u64,
            "function call finished",
        );
        result.map(Response::new)
    }

    async fn list_functions(
        &self,
        _request: Request<pb::ListFunctionsRequest>,
    ) -> Result<Response<pb::ListFunctionsResponse>, Status> {
        Ok(Response::new(pb::ListFunctionsResponse {
            functions: self.hub.list().await,
        }))
    }
}

pub struct GrpcProviderService {
    hub: FunctionHubHandle,
    functions: Arc<Functions>,
}

impl GrpcProviderService {
    pub fn new(hub: FunctionHubHandle, functions: Arc<Functions>) -> Self {
        Self { hub, functions }
    }
}

#[tonic::async_trait]
impl ProviderTransport for GrpcProviderService {
    type OpenProviderSessionStream =
        Pin<Box<dyn Stream<Item = Result<pb::ProviderServerFrame, Status>> + Send + 'static>>;

    async fn open_provider_session(
        &self,
        request: Request<tonic::Streaming<pb::ProviderClientFrame>>,
    ) -> Result<Response<Self::OpenProviderSessionStream>, Status> {
        let token = bearer_token(request.metadata()).map(str::to_owned);
        let mut incoming = request.into_inner();
        let first = incoming
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("hello frame required"))?;
        let Some(pb::provider_client_frame::Body::Hello(hello)) = first.body else {
            return Err(Status::invalid_argument("first frame must be hello"));
        };
        let provider_id = hello.provider_id;
        if !self.functions.authenticate(&provider_id, token.as_deref()) {
            return Err(Status::unauthenticated("unknown provider or wrong token"));
        }

        // Calls never exceed a function's concurrency and neither do cancels,
        // so the channel only fills when the provider stops reading.
        let capacity = hello
            .functions
            .iter()
            .map(|declaration| {
                let name = declaration
                    .function
                    .as_ref()
                    .map_or("", |function| function.name.as_str());
                let declared = declaration.max_concurrency.min(1024);
                2 * self.functions.max_concurrency(name, declared) as usize
            })
            .sum::<usize>()
            + OUTPUT_SLACK;
        let (output, output_rx) = mpsc::channel(capacity);
        let ticket = self
            .hub
            .register(provider_id.clone(), hello.functions, output)
            .await
            .map_err(|rejection| match rejection {
                RegisterRejection::Invalid(message) => Status::invalid_argument(message),
                RegisterRejection::NotPermitted { function } => Status::permission_denied(format!(
                    "provider {provider_id} may not register {function}"
                )),
                RegisterRejection::Owned { function, owner } => {
                    Status::already_exists(format!("{function} is served by provider {owner}"))
                }
                RegisterRejection::Stopped => Status::internal("function hub stopped"),
            })?;

        let hub = self.hub.clone();
        tokio::spawn(async move {
            while let Ok(Some(frame)) = incoming.message().await {
                match frame.body {
                    Some(pb::provider_client_frame::Body::Result(result)) => {
                        hub.result(ticket.clone(), result).await;
                    }
                    Some(pb::provider_client_frame::Body::Hello(_)) | None => {
                        warn!(
                            provider = %ticket.provider_id,
                            "closing provider stream after an unexpected frame",
                        );
                        break;
                    }
                }
            }
            info!(provider = %ticket.provider_id, "provider stream ended");
            hub.closed(ticket).await;
        });
        Ok(Response::new(Box::pin(
            ReceiverStream::new(output_rx).map(Ok),
        )))
    }
}

fn bearer_token(metadata: &MetadataMap) -> Option<&str> {
    metadata
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

/// The caller's deadline from its `grpc-timeout` header: one to eight ASCII
/// digits followed by a unit (`H`, `M`, `S`, `m`, `u`, `n`).
pub(crate) fn grpc_timeout(metadata: &MetadataMap) -> Option<Duration> {
    let value = metadata.get("grpc-timeout")?.to_str().ok()?;
    let (digits, unit) = value.split_at(value.len().checked_sub(1)?);
    if digits.is_empty() || digits.len() > 8 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let amount: u64 = digits.parse().ok()?;
    Some(match unit {
        "H" => Duration::from_secs(amount * 3600),
        "M" => Duration::from_secs(amount * 60),
        "S" => Duration::from_secs(amount),
        "m" => Duration::from_millis(amount),
        "u" => Duration::from_micros(amount),
        "n" => Duration::from_nanos(amount),
        _ => return None,
    })
}
