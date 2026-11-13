//! The SDK's media upload against the core's media service on a loopback
//! listener, behind the same token check the server applies.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use wuphf_connector::{MediaClient, SessionError};
use wuphf_model::MediaId;
use wuphf_proto::connector as pb;
use wuphf_proto::connector::media_transport_server::MediaTransportServer;
use wuphf_runtime::auth::any_token_auth;
use wuphf_runtime::media::GrpcMediaService;
use wuphf_storage::{InMemoryStorage, MediaStore};

const TOKEN: &str = "wa-token";

async fn serve(storage: Arc<InMemoryStorage>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let task = tokio::spawn(async move {
        Server::builder()
            .add_service(MediaTransportServer::with_interceptor(
                GrpcMediaService::new(storage, 1024 * 1024, Duration::from_secs(3600)),
                any_token_auth(vec![TOKEN.into()]),
            ))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (address, task)
}

fn photo(size: usize) -> pb::MediaMetadata {
    pb::MediaMetadata {
        kind: pb::attachment::Kind::Image.into(),
        file_name: Some("avatar.jpg".into()),
        mime_type: Some("image/jpeg".into()),
        size_bytes: Some(size as u64),
        animated: false,
    }
}

#[tokio::test]
async fn upload_stores_the_bytes_in_chunks() {
    let storage = Arc::new(InMemoryStorage::default());
    let (address, server) = serve(Arc::clone(&storage)).await;
    let client = MediaClient::connect(&address, TOKEN).unwrap();
    // More than two chunks, so the stream carries several chunk frames.
    let bytes: Vec<u8> = (0..150_000u32).map(|n| (n % 251) as u8).collect();

    let attachment = client
        .upload(photo(bytes.len()), bytes.clone())
        .await
        .unwrap();

    assert_eq!(attachment.size_bytes, Some(bytes.len() as u64));
    let media_id = MediaId(attachment.media_id.parse().unwrap());
    let stored = storage.media(media_id).await.unwrap().expect("stored");
    assert_eq!(stored.bytes, bytes);
    server.abort();
}

#[tokio::test]
async fn upload_with_a_wrong_token_is_refused() {
    let (address, server) = serve(Arc::new(InMemoryStorage::default())).await;
    let client = MediaClient::connect(&address, "not-the-token").unwrap();

    let error = client.upload(photo(3), b"abc".to_vec()).await.unwrap_err();

    assert!(
        matches!(&error, SessionError::Transport(status) if status.code() == tonic::Code::Unauthenticated),
        "{error:?}"
    );
    server.abort();
}
