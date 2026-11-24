//! End-to-end tests for the broker over a real TCP gRPC server, plus unit tests
//! for configuration resolution and header parsing. Events are ordered by
//! awaiting frames, replies and `ListFunctions` state, never by sleeping.

use crate::config::{
    ConfigError, FunctionOverride, Functions, FunctionsConfig, ProviderConfig, is_valid_name,
};
use crate::hub::FunctionHub;
use crate::service::{GrpcFunctionService, GrpcProviderService, grpc_timeout};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::{Channel, Server};
use tonic::{Code, Request, Status};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::function_transport_client::FunctionTransportClient;
use wuphf_proto::connector::function_transport_server::FunctionTransportServer;
use wuphf_proto::connector::provider_transport_client::ProviderTransportClient;
use wuphf_proto::connector::provider_transport_server::ProviderTransportServer;

/// How long a test waits for something that should happen promptly before it
/// fails instead of hanging.
const PATIENCE: Duration = Duration::from_secs(5);

/// A configuration where provider `id` may register `patterns`, reading its
/// token from `TOKEN_<id>`. Deadlines are long enough that only tests about
/// deadlines ever reach one.
fn config(providers: &[(&str, &[&str])]) -> FunctionsConfig {
    FunctionsConfig {
        default_deadline_ms: 5_000,
        max_deadline_ms: 10_000,
        providers: providers
            .iter()
            .map(|(id, patterns)| ProviderConfig {
                id: (*id).into(),
                token_env: format!("TOKEN_{id}"),
                functions: patterns.iter().map(|pattern| (*pattern).into()).collect(),
            })
            .collect(),
        ..FunctionsConfig::default()
    }
}

fn token(provider_id: &str) -> String {
    format!("secret-{provider_id}")
}

/// Serve both broker services on an ephemeral local port; returns its URI.
/// Every provider's token is `secret-<id>`.
async fn start(config: FunctionsConfig) -> String {
    let tokens: Vec<(String, String)> = config
        .providers
        .iter()
        .map(|provider| (provider.token_env.clone(), token(&provider.id)))
        .collect();
    let tokens: Vec<(&str, &str)> = tokens
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let functions = Arc::new(Functions::for_tests(config, &tokens));
    let (hub, handle) = FunctionHub::new(Arc::clone(&functions));
    tokio::spawn(hub.run());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(
        Server::builder()
            .add_service(FunctionTransportServer::new(GrpcFunctionService::new(
                handle.clone(),
                Arc::clone(&functions),
            )))
            .add_service(ProviderTransportServer::new(GrpcProviderService::new(
                handle, functions,
            )))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    format!("http://{address}")
}

fn function(name: &str, major: u32) -> pb::FunctionRef {
    pb::FunctionRef {
        name: name.into(),
        major,
    }
}

fn declare(name: &str) -> pb::FunctionDeclaration {
    pb::FunctionDeclaration {
        function: Some(function(name, 1)),
        request_schema: "{}".into(),
        response_schema: "{}".into(),
        idempotent: false,
        max_concurrency: 0,
        default_deadline_ms: None,
    }
}

/// A connected provider: frames it sends and the calls it receives.
struct Provider {
    frames: mpsc::Sender<pb::ProviderClientFrame>,
    stream: tonic::Streaming<pb::ProviderServerFrame>,
}

impl Provider {
    async fn connect(
        uri: &str,
        provider_id: &str,
        token: &str,
        functions: Vec<pb::FunctionDeclaration>,
    ) -> Result<Self, Status> {
        let (frames, receiver) = mpsc::channel(16);
        frames
            .send(pb::ProviderClientFrame {
                body: Some(pb::provider_client_frame::Body::Hello(pb::ProviderHello {
                    provider_id: provider_id.into(),
                    functions,
                    tools: Vec::new(),
                })),
            })
            .await
            .unwrap();
        let mut request = Request::new(ReceiverStream::new(receiver));
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        let mut client = ProviderTransportClient::connect(uri.to_owned())
            .await
            .unwrap();
        let stream = client.open_provider_session(request).await?.into_inner();
        Ok(Self { frames, stream })
    }

    /// The next frame, or `None` once the core ends the session.
    async fn next_frame(&mut self) -> Option<pb::ProviderServerFrame> {
        tokio::time::timeout(PATIENCE, self.stream.message())
            .await
            .expect("provider frame within patience")
            .ok()
            .flatten()
    }

    async fn next_call(&mut self) -> pb::ProviderCall {
        match self.next_frame().await.and_then(|frame| frame.body) {
            Some(pb::provider_server_frame::Body::Call(call)) => call,
            other => panic!("expected a call, got {other:?}"),
        }
    }

    async fn send(&self, call_id: &str, outcome: pb::call_result::Outcome) {
        self.frames
            .send(pb::ProviderClientFrame {
                body: Some(pb::provider_client_frame::Body::Result(pb::CallResult {
                    call_id: call_id.into(),
                    outcome: Some(outcome),
                })),
            })
            .await
            .unwrap();
    }

    async fn reply(&self, call_id: &str, result: &str, media_ids: &[&str]) {
        self.send(
            call_id,
            pb::call_result::Outcome::Success(pb::CallSuccess {
                result: result.as_bytes().to_vec(),
                media_ids: media_ids.iter().map(|id| (*id).into()).collect(),
            }),
        )
        .await;
    }

    async fn fail(&self, call_id: &str, code: pb::CallErrorCode, message: &str) {
        self.send(
            call_id,
            pb::call_result::Outcome::Error(pb::CallError {
                code: code.into(),
                message: message.into(),
            }),
        )
        .await;
    }
}

async fn caller(uri: &str) -> FunctionTransportClient<Channel> {
    FunctionTransportClient::connect(uri.to_owned())
        .await
        .unwrap()
}

fn invoke_request(name: &str, args: &str) -> pb::InvokeRequest {
    pb::InvokeRequest {
        function: Some(function(name, 1)),
        args: args.as_bytes().to_vec(),
        media_ids: vec![],
        caller: "test".into(),
    }
}

/// Invoke in the background so the test can play the provider meanwhile.
fn spawn_invoke(
    client: &FunctionTransportClient<Channel>,
    request: pb::InvokeRequest,
) -> tokio::task::JoinHandle<Result<pb::InvokeResponse, Status>> {
    let mut client = client.clone();
    tokio::spawn(async move {
        client
            .invoke(request)
            .await
            .map(tonic::Response::into_inner)
    })
}

async fn list(client: &mut FunctionTransportClient<Channel>) -> Vec<pb::FunctionStatus> {
    client
        .list_functions(pb::ListFunctionsRequest {})
        .await
        .unwrap()
        .into_inner()
        .functions
}

fn status_of<'a>(statuses: &'a [pb::FunctionStatus], name: &str) -> Option<&'a pb::FunctionStatus> {
    statuses.iter().find(|status| {
        status
            .function
            .as_ref()
            .is_some_and(|function| function.name == name)
    })
}

/// Re-query `ListFunctions` until `name`'s status satisfies `predicate`.
async fn wait_for(
    client: &FunctionTransportClient<Channel>,
    name: &str,
    predicate: impl Fn(&pb::FunctionStatus) -> bool,
) {
    let mut client = client.clone();
    tokio::time::timeout(PATIENCE, async {
        loop {
            if status_of(&list(&mut client).await, name).is_some_and(&predicate) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("function state reached within patience");
}

async fn finished(
    handle: tokio::task::JoinHandle<Result<pb::InvokeResponse, Status>>,
) -> Result<pb::InvokeResponse, Status> {
    tokio::time::timeout(PATIENCE, handle)
        .await
        .expect("invoke finished within patience")
        .unwrap()
}

#[tokio::test]
async fn invoke_returns_provider_result() {
    let uri = start(config(&[("echo", &["test.*"])])).await;
    let mut provider = Provider::connect(&uri, "echo", &token("echo"), vec![declare("test.echo")])
        .await
        .unwrap();
    let client = caller(&uri).await;
    let mut request = invoke_request("test.echo", r#"{"x":1}"#);
    request.media_ids = vec!["media-in".into()];
    request.caller = "connector-a".into();
    let invoked = spawn_invoke(&client, request);

    let call = provider.next_call().await;
    assert_eq!(call.args, br#"{"x":1}"#);
    assert_eq!(call.media_ids, ["media-in"]);
    assert_eq!(call.caller, "connector-a");
    assert_eq!(call.function, Some(function("test.echo", 1)));
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert!(call.deadline_unix_ms > now_ms);
    provider
        .reply(&call.call_id, r#"{"y":2}"#, &["media-out"])
        .await;

    let response = finished(invoked).await.unwrap();
    assert_eq!(response.result, br#"{"y":2}"#);
    assert_eq!(response.media_ids, ["media-out"]);
    assert_eq!(response.provider_id, "echo");
}

#[tokio::test]
async fn invoke_waits_for_a_provider_that_registers_later() {
    let uri = start(config(&[("echo", &["test.echo"])])).await;
    let client = caller(&uri).await;
    let invoked = spawn_invoke(&client, invoke_request("test.echo", "{}"));
    wait_for(&client, "test.echo", |status| status.queued == 1).await;

    let mut provider = Provider::connect(&uri, "echo", &token("echo"), vec![declare("test.echo")])
        .await
        .unwrap();
    let call = provider.next_call().await;
    provider.reply(&call.call_id, "true", &[]).await;
    assert_eq!(finished(invoked).await.unwrap().result, b"true");
}

#[tokio::test]
async fn invoke_for_unservable_name_is_not_found_immediately() {
    let uri = start(config(&[("echo", &["test.*"])])).await;
    let mut client = caller(&uri).await;
    let started = Instant::now();
    let status = client
        .invoke(invoke_request("other.thing", "{}"))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::NotFound);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn invoke_without_provider_times_out_with_reason() {
    let uri = start(FunctionsConfig {
        default_deadline_ms: 300,
        max_deadline_ms: 600,
        ..config(&[("echo", &["test.*"])])
    })
    .await;
    let mut client = caller(&uri).await;
    let status = client
        .invoke(invoke_request("test.echo", "{}"))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::DeadlineExceeded);
    assert!(status.message().contains("no provider"), "{status:?}");
}

#[tokio::test]
async fn requested_deadline_is_capped_by_max() {
    let uri = start(FunctionsConfig {
        default_deadline_ms: 300,
        max_deadline_ms: 300,
        ..config(&[("echo", &["test.*"])])
    })
    .await;
    let mut client = caller(&uri).await;
    let mut request = Request::new(invoke_request("test.echo", "{}"));
    request.set_timeout(Duration::from_secs(10));
    let started = Instant::now();
    let status = client.invoke(request).await.unwrap_err();
    assert_eq!(status.code(), Code::DeadlineExceeded);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn override_default_deadline_wins_over_provider_suggestion() {
    let mut configuration = config(&[("echo", &["*"])]);
    configuration.overrides.insert(
        "test.slow".into(),
        FunctionOverride {
            default_deadline_ms: Some(7_000),
            max_deadline_ms: Some(8_000),
            ..FunctionOverride::default()
        },
    );
    let functions = Functions::for_tests(configuration, &[("TOKEN_echo", "secret-echo")]);
    let ms = Duration::from_millis;

    // Without an override: request, else suggestion, else global default; capped.
    assert_eq!(functions.deadline("test.fast", None, None), ms(5_000));
    assert_eq!(
        functions.deadline("test.fast", None, Some(ms(2_000))),
        ms(2_000)
    );
    assert_eq!(
        functions.deadline("test.fast", Some(ms(1_000)), Some(ms(2_000))),
        ms(1_000)
    );
    assert_eq!(
        functions.deadline("test.fast", Some(ms(60_000)), None),
        ms(10_000)
    );
    assert_eq!(
        functions.deadline("test.fast", None, Some(ms(60_000))),
        ms(10_000)
    );
    // With an override: its default beats the suggestion, its max caps.
    assert_eq!(
        functions.deadline("test.slow", None, Some(ms(2_000))),
        ms(7_000)
    );
    assert_eq!(
        functions.deadline("test.slow", Some(ms(9_000)), None),
        ms(8_000)
    );
    assert_eq!(
        functions.deadline("test.slow", Some(ms(500)), None),
        ms(500)
    );
}

#[tokio::test]
async fn other_provider_claiming_owned_function_is_rejected() {
    let uri = start(config(&[("first", &["test.*"]), ("second", &["test.*"])])).await;
    let _first = Provider::connect(&uri, "first", &token("first"), vec![declare("test.echo")])
        .await
        .unwrap();
    let Err(status) =
        Provider::connect(&uri, "second", &token("second"), vec![declare("test.echo")]).await
    else {
        panic!("a second provider must not claim test.echo");
    };
    assert_eq!(status.code(), Code::AlreadyExists);
    assert!(status.message().contains("first"), "{status:?}");
}

#[tokio::test]
async fn same_provider_reconnecting_takes_over() {
    let uri = start(config(&[("echo", &["test.*"])])).await;
    let mut old = Provider::connect(&uri, "echo", &token("echo"), vec![declare("test.echo")])
        .await
        .unwrap();
    let mut new = Provider::connect(&uri, "echo", &token("echo"), vec![declare("test.echo")])
        .await
        .unwrap();
    assert!(old.next_frame().await.is_none(), "old session must end");

    let client = caller(&uri).await;
    let invoked = spawn_invoke(&client, invoke_request("test.echo", "{}"));
    let call = new.next_call().await;
    new.reply(&call.call_id, "1", &[]).await;
    assert_eq!(finished(invoked).await.unwrap().result, b"1");
}

#[tokio::test]
async fn wrong_token_is_unauthenticated() {
    let uri = start(config(&[("echo", &["test.*"])])).await;
    for (provider_id, token) in [("echo", "wrong"), ("stranger", "secret-stranger")] {
        let Err(status) =
            Provider::connect(&uri, provider_id, token, vec![declare("test.echo")]).await
        else {
            panic!("{provider_id} must not authenticate");
        };
        assert_eq!(status.code(), Code::Unauthenticated);
    }
}

#[tokio::test]
async fn function_outside_patterns_is_permission_denied() {
    let uri = start(config(&[("echo", &["test.*"])])).await;
    let Err(status) = Provider::connect(
        &uri,
        "echo",
        &token("echo"),
        vec![declare("test.echo"), declare("media.convert")],
    )
    .await
    else {
        panic!("echo may not register media.convert");
    };
    assert_eq!(status.code(), Code::PermissionDenied);
    // The whole registration was rejected, so test.echo has no owner either.
    let statuses = list(&mut caller(&uri).await).await;
    assert!(status_of(&statuses, "test.echo").is_none());
}

#[tokio::test]
async fn idempotent_call_is_requeued_when_provider_drops() {
    let uri = start(config(&[("echo", &["test.*"])])).await;
    let declaration = pb::FunctionDeclaration {
        idempotent: true,
        ..declare("test.echo")
    };
    let mut first = Provider::connect(&uri, "echo", &token("echo"), vec![declaration.clone()])
        .await
        .unwrap();
    let client = caller(&uri).await;
    let invoked = spawn_invoke(&client, invoke_request("test.echo", "{}"));
    let lost = first.next_call().await;
    drop(first);

    let mut second = Provider::connect(&uri, "echo", &token("echo"), vec![declaration])
        .await
        .unwrap();
    let retried = second.next_call().await;
    assert_eq!(retried.call_id, lost.call_id);
    second.reply(&retried.call_id, r#""second""#, &[]).await;
    assert_eq!(finished(invoked).await.unwrap().result, br#""second""#);
}

#[tokio::test]
async fn non_idempotent_call_fails_when_provider_drops() {
    let uri = start(config(&[("echo", &["test.*"])])).await;
    let mut provider = Provider::connect(&uri, "echo", &token("echo"), vec![declare("test.echo")])
        .await
        .unwrap();
    let client = caller(&uri).await;
    let invoked = spawn_invoke(&client, invoke_request("test.echo", "{}"));
    provider.next_call().await;
    drop(provider);

    let status = finished(invoked).await.unwrap_err();
    assert_eq!(status.code(), Code::Unavailable);
    assert!(status.message().contains("echo"), "{status:?}");
}

#[tokio::test]
async fn caller_cancel_sends_cancel_to_provider() {
    let uri = start(config(&[("echo", &["test.*"])])).await;
    let mut provider = Provider::connect(&uri, "echo", &token("echo"), vec![declare("test.echo")])
        .await
        .unwrap();
    let mut client = caller(&uri).await;
    let mut request = Request::new(invoke_request("test.echo", "{}"));
    request.set_timeout(Duration::from_millis(100));
    let invoked = {
        let mut client = client.clone();
        tokio::spawn(async move { client.invoke(request).await })
    };

    let held = provider.next_call().await;
    let Some(pb::provider_server_frame::Body::Cancel(cancel)) =
        provider.next_frame().await.and_then(|frame| frame.body)
    else {
        panic!("provider must receive a cancel");
    };
    assert_eq!(cancel.call_id, held.call_id);
    assert!(finished_raw(invoked).await.is_err());

    // A result after the cancel is ignored; the next call still works.
    provider.reply(&held.call_id, "\"late\"", &[]).await;
    let invoked = spawn_invoke(&client, invoke_request("test.echo", "{}"));
    let next = provider.next_call().await;
    assert_ne!(next.call_id, held.call_id);
    provider.reply(&next.call_id, "\"fresh\"", &[]).await;
    assert_eq!(finished(invoked).await.unwrap().result, b"\"fresh\"");

    let statuses = list(&mut client).await;
    let status = status_of(&statuses, "test.echo").unwrap();
    assert_eq!((status.completed, status.abandoned), (1, 1));
    assert_eq!(status.in_flight, 0);
}

async fn finished_raw<T>(handle: tokio::task::JoinHandle<T>) -> T {
    tokio::time::timeout(PATIENCE, handle)
        .await
        .expect("task finished within patience")
        .unwrap()
}

#[tokio::test]
async fn concurrency_limit_queues_extra_calls() {
    let uri = start(config(&[("echo", &["test.*"])])).await;
    let declaration = pb::FunctionDeclaration {
        max_concurrency: 1,
        ..declare("test.echo")
    };
    let mut provider = Provider::connect(&uri, "echo", &token("echo"), vec![declaration])
        .await
        .unwrap();
    let client = caller(&uri).await;
    let first = spawn_invoke(&client, invoke_request("test.echo", "1"));
    let second = spawn_invoke(&client, invoke_request("test.echo", "2"));

    let call = provider.next_call().await;
    wait_for(&client, "test.echo", |status| {
        status.in_flight == 1 && status.queued == 1
    })
    .await;
    provider
        .reply(&call.call_id, &String::from_utf8(call.args).unwrap(), &[])
        .await;
    let call = provider.next_call().await;
    provider
        .reply(&call.call_id, &String::from_utf8(call.args).unwrap(), &[])
        .await;

    assert_eq!(finished(first).await.unwrap().result, b"1");
    assert_eq!(finished(second).await.unwrap().result, b"2");
}

#[tokio::test]
async fn full_queue_is_resource_exhausted() {
    let uri = start(FunctionsConfig {
        max_queued_calls: 1,
        ..config(&[("echo", &["test.*"])])
    })
    .await;
    let mut client = caller(&uri).await;
    let waiting = spawn_invoke(&client, invoke_request("test.echo", "{}"));
    wait_for(&client, "test.echo", |status| status.queued == 1).await;

    let started = Instant::now();
    let status = client
        .invoke(invoke_request("test.echo", "{}"))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::ResourceExhausted);
    assert!(started.elapsed() < Duration::from_secs(1));
    waiting.abort();
}

#[tokio::test]
async fn oversized_or_non_json_args_are_invalid() {
    let uri = start(FunctionsConfig {
        max_body_bytes: 16,
        ..config(&[("echo", &["test.*"])])
    })
    .await;
    let mut client = caller(&uri).await;
    let oversized = client
        .invoke(invoke_request("test.echo", r#"{"text":"far too long"}"#))
        .await
        .unwrap_err();
    assert_eq!(oversized.code(), Code::InvalidArgument);
    assert!(oversized.message().contains("media_ids"), "{oversized:?}");
    let malformed = client
        .invoke(invoke_request("test.echo", "not json"))
        .await
        .unwrap_err();
    assert_eq!(malformed.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn provider_error_codes_map_to_status() {
    let uri = start(config(&[("echo", &["test.*"])])).await;
    let mut provider = Provider::connect(&uri, "echo", &token("echo"), vec![declare("test.echo")])
        .await
        .unwrap();
    let client = caller(&uri).await;
    for (code, expected) in [
        (pb::CallErrorCode::InvalidArgument, Code::InvalidArgument),
        (pb::CallErrorCode::Unsupported, Code::Unimplemented),
        (pb::CallErrorCode::Failed, Code::Internal),
        (pb::CallErrorCode::Unspecified, Code::Internal),
    ] {
        let invoked = spawn_invoke(&client, invoke_request("test.echo", "{}"));
        let call = provider.next_call().await;
        provider.fail(&call.call_id, code, "nope").await;
        let status = finished(invoked).await.unwrap_err();
        assert_eq!(status.code(), expected, "{code:?}");
        assert!(status.message().contains("nope"), "{status:?}");
    }
}

#[tokio::test]
async fn list_functions_reports_provider_load_and_counters() {
    let uri = start(config(&[("echo", &["test.*"]), ("absent", &["other.*"])])).await;
    let declaration = pb::FunctionDeclaration {
        max_concurrency: 3,
        ..declare("test.echo")
    };
    let mut provider = Provider::connect(&uri, "echo", &token("echo"), vec![declaration])
        .await
        .unwrap();
    let mut client = caller(&uri).await;

    let invoked = spawn_invoke(&client, invoke_request("test.echo", "{}"));
    let call = provider.next_call().await;
    provider.reply(&call.call_id, "{}", &[]).await;
    finished(invoked).await.unwrap();
    let invoked = spawn_invoke(&client, invoke_request("test.echo", "{}"));
    let call = provider.next_call().await;
    provider
        .fail(&call.call_id, pb::CallErrorCode::Failed, "x")
        .await;
    finished(invoked).await.unwrap_err();
    let held = spawn_invoke(&client, invoke_request("test.echo", "{}"));
    provider.next_call().await;
    let queued = spawn_invoke(&client, invoke_request("other.thing", "{}"));
    wait_for(&client, "other.thing", |status| status.queued == 1).await;

    let statuses = list(&mut client).await;
    let names: Vec<_> = statuses
        .iter()
        .map(|status| status.function.as_ref().unwrap().name.as_str())
        .collect();
    assert_eq!(names, ["other.thing", "test.echo"]);
    let echo = status_of(&statuses, "test.echo").unwrap();
    assert_eq!(echo.provider_id, "echo");
    assert!(echo.declaration.is_some());
    assert_eq!(
        (echo.in_flight, echo.max_concurrency, echo.queued),
        (1, 3, 0)
    );
    assert_eq!((echo.completed, echo.failed, echo.abandoned), (1, 1, 0));
    let other = status_of(&statuses, "other.thing").unwrap();
    assert_eq!(other.provider_id, "");
    assert!(other.declaration.is_none());
    assert_eq!((other.queued, other.max_concurrency), (1, 0));
    held.abort();
    queued.abort();
}

fn resolve(configuration: FunctionsConfig) -> Result<Functions, ConfigError> {
    configuration.resolve(|variable| (variable == "TOKEN_echo").then(|| "secret-echo".into()))
}

#[test]
fn configuration_rejections_name_the_offending_item() {
    let valid = || config(&[("echo", &["test.*"])]);
    assert!(resolve(valid()).is_ok());

    let zero = |field: &str| ConfigError::Zero {
        field: field.into(),
    };
    for (configuration, field) in [
        (
            FunctionsConfig {
                default_deadline_ms: 0,
                ..valid()
            },
            "functions.default_deadline_ms",
        ),
        (
            FunctionsConfig {
                max_deadline_ms: 0,
                ..valid()
            },
            "functions.max_deadline_ms",
        ),
        (
            FunctionsConfig {
                keepalive_interval_ms: 0,
                ..valid()
            },
            "functions.keepalive_interval_ms",
        ),
        (
            FunctionsConfig {
                keepalive_timeout_ms: 0,
                ..valid()
            },
            "functions.keepalive_timeout_ms",
        ),
        (
            FunctionsConfig {
                max_body_bytes: 0,
                ..valid()
            },
            "functions.max_body_bytes",
        ),
        (
            FunctionsConfig {
                max_queued_calls: 0,
                ..valid()
            },
            "functions.max_queued_calls",
        ),
    ] {
        assert_eq!(resolve(configuration).err(), Some(zero(field)));
    }
    assert!(matches!(
        resolve(FunctionsConfig {
            default_deadline_ms: 2,
            max_deadline_ms: 1,
            ..valid()
        }),
        Err(ConfigError::DefaultAboveMax { .. })
    ));

    let with_override = |entry: FunctionOverride| {
        let mut configuration = valid();
        configuration.overrides.insert("test.echo".into(), entry);
        resolve(configuration).err()
    };
    assert_eq!(
        with_override(FunctionOverride {
            max_concurrency: Some(0),
            ..FunctionOverride::default()
        }),
        Some(zero("functions.overrides.\"test.echo\".max_concurrency"))
    );
    // An override's default is checked against its own max, else the global one.
    assert!(matches!(
        with_override(FunctionOverride {
            default_deadline_ms: Some(3_000),
            max_deadline_ms: Some(2_000),
            ..FunctionOverride::default()
        }),
        Some(ConfigError::DefaultAboveMax { .. })
    ));
    assert!(matches!(
        with_override(FunctionOverride {
            default_deadline_ms: Some(20_000),
            ..FunctionOverride::default()
        }),
        Some(ConfigError::DefaultAboveMax { .. })
    ));
    let mut bad_key = valid();
    bad_key
        .overrides
        .insert("Test".into(), FunctionOverride::default());
    assert_eq!(
        resolve(bad_key).err(),
        Some(ConfigError::InvalidOverride("Test".into()))
    );

    let with_providers = |providers: &[(&str, &[&str])]| resolve(config(providers)).err();
    assert_eq!(
        with_providers(&[("", &["test.*"])]),
        Some(ConfigError::EmptyProviderId)
    );
    assert_eq!(
        with_providers(&[("echo", &["test.*"]), ("echo", &["other.*"])]),
        Some(ConfigError::DuplicateProviderId("echo".into()))
    );
    assert_eq!(
        with_providers(&[("echo", &[])]),
        Some(ConfigError::NoFunctions("echo".into()))
    );
    for pattern in ["test", "Test.echo", "test.*.x", "test.", "*.echo", ""] {
        assert_eq!(
            with_providers(&[("echo", &[pattern])]),
            Some(ConfigError::InvalidPattern {
                provider: "echo".into(),
                pattern: pattern.into(),
            }),
            "{pattern:?}"
        );
    }
    assert_eq!(
        with_providers(&[("other", &["test.*"])]),
        Some(ConfigError::MissingToken {
            owner: "function provider other".into(),
            variable: "TOKEN_other".into(),
        })
    );
    let unset_invoke_token = FunctionsConfig {
        invoke_token_env: Some("INVOKE".into()),
        ..valid()
    };
    assert!(matches!(
        resolve(unset_invoke_token).err(),
        Some(ConfigError::MissingToken { .. })
    ));
    let empty_token = config(&[("echo", &["test.*"])]).resolve(|_| Some(String::new()));
    assert!(matches!(
        empty_token.err(),
        Some(ConfigError::MissingToken { .. })
    ));
}

#[test]
fn provider_patterns_match_names() {
    let functions = Functions::for_tests(
        config(&[
            ("any", &["*"]),
            ("media", &["media.*"]),
            ("exact", &["text.render"]),
        ]),
        &[
            ("TOKEN_any", "a"),
            ("TOKEN_media", "m"),
            ("TOKEN_exact", "e"),
        ],
    );
    assert!(functions.may_register("any", "whatever.name"));
    assert!(functions.may_register("media", "media.convert"));
    assert!(functions.may_register("media", "media.image.resize"));
    assert!(!functions.may_register("media", "mediax.y"));
    assert!(functions.may_register("exact", "text.render"));
    assert!(!functions.may_register("exact", "text.render2"));
    assert!(!functions.may_register("unknown", "text.render"));
    assert!(functions.authenticate("media", Some("m")));
    assert!(!functions.authenticate("media", Some("a")));
    assert!(!functions.authenticate("media", None));
}

#[test]
fn function_names_need_lowercase_dotted_segments() {
    for name in ["media.convert", "a.b.c", "x1.y_2"] {
        assert!(is_valid_name(name), "{name}");
    }
    for name in [
        "media",
        "Media.convert",
        "media.",
        ".convert",
        "1a.b",
        "a..b",
        "a.b-c",
    ] {
        assert!(!is_valid_name(name), "{name}");
    }
}

#[test]
fn grpc_timeout_header_parses() {
    let parse = |value: &str| {
        let mut metadata = tonic::metadata::MetadataMap::new();
        metadata.insert("grpc-timeout", value.parse().unwrap());
        grpc_timeout(&metadata)
    };
    assert_eq!(parse("100m"), Some(Duration::from_millis(100)));
    assert_eq!(parse("2S"), Some(Duration::from_secs(2)));
    assert_eq!(parse("1H"), Some(Duration::from_secs(3600)));
    assert_eq!(parse("5M"), Some(Duration::from_secs(300)));
    assert_eq!(parse("7u"), Some(Duration::from_micros(7)));
    assert_eq!(parse("9n"), Some(Duration::from_nanos(9)));
    for malformed in ["", "m", "100", "100x", "123456789m", "-1m", "1.5S"] {
        assert_eq!(parse(malformed), None, "{malformed:?}");
    }
    assert_eq!(grpc_timeout(&tonic::metadata::MetadataMap::new()), None);
}
