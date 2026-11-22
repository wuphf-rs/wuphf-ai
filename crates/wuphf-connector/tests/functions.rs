//! The SDK's functions client and media.convert against a scripted
//! FunctionTransport and the real media service on one loopback listener.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use wuphf_connector::{
    FunctionError, FunctionsClient, MediaClient, MediaConvertArgs, MediaConverter,
};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::function_transport_server::{
    FunctionTransport, FunctionTransportServer,
};
use wuphf_proto::connector::media_transport_server::MediaTransportServer;
use wuphf_runtime::auth::any_token_auth;
use wuphf_runtime::media::GrpcMediaService;
use wuphf_storage::InMemoryStorage;

const MEDIA_TOKEN: &str = "wa-token";

/// What the fake answers an invoke with, and how long it waits first.
#[derive(Clone, Default)]
struct Script {
    result: Vec<u8>,
    media_ids: Vec<String>,
    delay: Duration,
}

/// Each request the fake saw, with its authorization header.
type Seen = Arc<Mutex<Vec<(pb::InvokeRequest, Option<String>)>>>;

/// A FunctionTransport that records each request (with its authorization
/// header) and answers from its script.
#[derive(Clone, Default)]
struct FakeFunctions {
    script: Arc<Mutex<Script>>,
    seen: Seen,
}

#[tonic::async_trait]
impl FunctionTransport for FakeFunctions {
    async fn invoke(
        &self,
        request: Request<pb::InvokeRequest>,
    ) -> Result<Response<pb::InvokeResponse>, Status> {
        let authorization = request
            .metadata()
            .get("authorization")
            .map(|value| value.to_str().unwrap().to_owned());
        self.seen
            .lock()
            .unwrap()
            .push((request.into_inner(), authorization));
        let script = self.script.lock().unwrap().clone();
        tokio::time::sleep(script.delay).await;
        Ok(Response::new(pb::InvokeResponse {
            result: script.result,
            media_ids: script.media_ids,
            provider_id: "mediaconv".into(),
        }))
    }

    async fn list_functions(
        &self,
        _request: Request<pb::ListFunctionsRequest>,
    ) -> Result<Response<pb::ListFunctionsResponse>, Status> {
        Err(Status::unimplemented("not in this test"))
    }
}

async fn serve(functions: FakeFunctions) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let task = tokio::spawn(async move {
        Server::builder()
            .add_service(FunctionTransportServer::new(functions))
            .add_service(MediaTransportServer::with_interceptor(
                GrpcMediaService::new(
                    Arc::new(InMemoryStorage::default()),
                    1024 * 1024,
                    Duration::from_secs(3600),
                ),
                any_token_auth(vec![MEDIA_TOKEN.into()]),
            ))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (address, task)
}

fn script(functions: &FakeFunctions, result: &str, media_ids: &[&str]) {
    *functions.script.lock().unwrap() = Script {
        result: result.as_bytes().to_vec(),
        media_ids: media_ids.iter().map(|id| (*id).to_owned()).collect(),
        delay: Duration::ZERO,
    };
}

#[derive(Debug, serde::Deserialize, PartialEq)]
struct Echo {
    ok: bool,
}

#[tokio::test]
async fn invoke_sends_json_args_media_and_caller() {
    let functions = FakeFunctions::default();
    script(&functions, r#"{"ok":true}"#, &[]);
    let (address, server) = serve(functions.clone()).await;
    let with_token = FunctionsClient::connect(&address, Some("t"), "whatsapp-rust").unwrap();
    let args = serde_json::json!({"a": 1});
    let _: (Echo, _) = with_token
        .invoke("echo", 2, &args, vec!["m-1".into()], None)
        .await
        .unwrap();
    let without_token = FunctionsClient::connect(&address, None, "whatsapp-rust").unwrap();
    let _: (Echo, _) = without_token
        .invoke("echo", 2, &args, vec![], None)
        .await
        .unwrap();

    let seen = functions.seen.lock().unwrap();
    let (request, authorization) = &seen[0];
    let function = request.function.as_ref().unwrap();
    assert_eq!((function.name.as_str(), function.major), ("echo", 2));
    assert_eq!(request.args, br#"{"a":1}"#);
    assert_eq!(request.media_ids, ["m-1"]);
    assert_eq!(request.caller, "whatsapp-rust");
    assert_eq!(authorization.as_deref(), Some("Bearer t"));
    // No token, no header.
    assert_eq!(seen[1].1, None);
    server.abort();
}

#[tokio::test]
async fn invoke_decodes_the_result() {
    let functions = FakeFunctions::default();
    script(&functions, r#"{"ok":true}"#, &["r-1", "r-2"]);
    let (address, server) = serve(functions).await;
    let client = FunctionsClient::connect(&address, None, "test").unwrap();
    let (result, ids): (Echo, _) = client
        .invoke("echo", 1, &serde_json::json!({}), vec![], None)
        .await
        .unwrap();
    assert_eq!(result, Echo { ok: true });
    assert_eq!(ids, ["r-1", "r-2"]);
    server.abort();
}

#[tokio::test]
async fn an_undecodable_result_names_the_provider() {
    let functions = FakeFunctions::default();
    script(&functions, "not json", &[]);
    let (address, server) = serve(functions).await;
    let client = FunctionsClient::connect(&address, None, "test").unwrap();
    let error = client
        .invoke::<_, Echo>("echo", 1, &serde_json::json!({}), vec![], None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, FunctionError::Decode { provider, .. } if provider == "mediaconv"),
        "{error:?}"
    );
    server.abort();
}

#[test]
fn media_convert_args_encode_like_go() {
    let bare = MediaConvertArgs {
        source_mime_type: String::new(),
        source_file_name: String::new(),
        target_mime_type: "image/webp".into(),
    };
    assert_eq!(
        serde_json::to_string(&bare).unwrap(),
        r#"{"target_mime_type":"image/webp"}"#
    );
    let full = MediaConvertArgs {
        source_mime_type: "video/webm".into(),
        source_file_name: "s.webm".into(),
        ..bare
    };
    assert_eq!(
        serde_json::to_string(&full).unwrap(),
        r#"{"source_mime_type":"video/webm","source_file_name":"s.webm","target_mime_type":"image/webp"}"#
    );
}

fn converter(address: &str, timeout: Duration) -> MediaConverter {
    MediaConverter {
        functions: FunctionsClient::connect(address, None, "whatsapp-rust").unwrap(),
        media: MediaClient::connect(address, MEDIA_TOKEN).unwrap(),
        timeout: Arc::new(move || timeout),
    }
}

async fn stored(address: &str, bytes: &[u8]) -> String {
    let metadata = pb::MediaMetadata {
        kind: pb::attachment::Kind::Sticker.into(),
        mime_type: Some("image/webp".into()),
        size_bytes: Some(bytes.len() as u64),
        ..Default::default()
    };
    MediaClient::connect(address, MEDIA_TOKEN)
        .unwrap()
        .upload(metadata, bytes.to_vec())
        .await
        .unwrap()
        .media_id
}

#[tokio::test]
async fn convert_downloads_the_converted_media() {
    let functions = FakeFunctions::default();
    let (address, server) = serve(functions.clone()).await;
    let converted = stored(&address, b"webp").await;
    script(
        &functions,
        r#"{"mime_type":"image/webp","file_name":"s.webp"}"#,
        &[&converted],
    );

    let bytes = converter(&address, Duration::from_secs(5))
        .convert("source-id", "video/webm", "s.webm", "image/webp")
        .await
        .unwrap();

    assert_eq!(bytes, b"webp");
    let seen = functions.seen.lock().unwrap();
    let function = seen[0].0.function.as_ref().unwrap();
    assert_eq!(
        (function.name.as_str(), function.major),
        ("media.convert", 1)
    );
    assert_eq!(seen[0].0.media_ids, ["source-id"]);
    server.abort();
}

#[tokio::test]
async fn convert_wants_exactly_one_result() {
    let functions = FakeFunctions::default();
    script(
        &functions,
        r#"{"mime_type":"image/webp","file_name":"s.webp"}"#,
        &["a", "b"],
    );
    let (address, server) = serve(functions).await;
    let error = converter(&address, Duration::from_secs(5))
        .convert("source-id", "video/webm", "", "image/webp")
        .await
        .unwrap_err();
    assert!(
        matches!(error, FunctionError::MediaCount { count: 2, .. }),
        "{error:?}"
    );
    server.abort();
}

#[tokio::test]
async fn convert_honours_its_timeout() {
    let functions = FakeFunctions::default();
    *functions.script.lock().unwrap() = Script {
        delay: Duration::from_secs(2),
        ..Script::default()
    };
    let (address, server) = serve(functions).await;
    let started = std::time::Instant::now();
    let error = converter(&address, Duration::from_millis(100))
        .convert("source-id", "video/webm", "", "image/webp")
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(matches!(error, FunctionError::Timeout { .. }), "{error:?}");
    server.abort();
}
