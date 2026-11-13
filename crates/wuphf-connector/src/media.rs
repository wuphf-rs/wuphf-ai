//! Uploads to the core's media store: the counterpart of the Go
//! `coreclient.DialMedia` / `MediaClient.Upload`. A connector uploads the
//! bytes of an attachment (or a chat's picture) here first, then names the
//! returned media id in the frame that carries it.

use thiserror::Error;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::transport::{Channel, Endpoint};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::media_transport_client::MediaTransportClient;

use crate::client::{SessionError, core_uri};

/// The largest chunk one upload frame carries, as in the Go client.
const UPLOAD_CHUNK_BYTES: usize = 64 * 1024;

/// Why a [`MediaClient`] could not be built: only its inputs can be wrong,
/// since it connects on first use.
#[derive(Debug, Error)]
pub enum MediaConnectError {
    #[error("invalid core address: {0}")]
    Address(#[from] tonic::codegen::http::uri::InvalidUri),
    #[error("invalid core endpoint: {0}")]
    Endpoint(#[from] tonic::transport::Error),
    #[error("the core token is not a valid header value")]
    Token(#[from] tonic::metadata::errors::InvalidMetadataValue),
}

/// A client of the core's media service, authenticated with the connector's
/// core token. Cheap to clone.
#[derive(Clone)]
pub struct MediaClient {
    client: MediaTransportClient<Channel>,
    authorization: MetadataValue<Ascii>,
}

impl MediaClient {
    /// A client for the core at `core_address` (a scheme is added when
    /// missing). The connection is made on first use, as the Go client does,
    /// so a connector can build it before the core is up.
    pub fn connect(core_address: &str, token: &str) -> Result<Self, MediaConnectError> {
        let channel = Endpoint::new(core_uri(core_address)?)?.connect_lazy();
        Ok(Self {
            client: MediaTransportClient::new(channel),
            authorization: format!("Bearer {token}").parse()?,
        })
    }

    /// Upload `bytes` described by `metadata`: one metadata frame, then the
    /// bytes in chunks of at most 64 KiB. Returns the stored attachment, whose
    /// media id the connector names in its frames.
    pub async fn upload(
        &self,
        metadata: pb::MediaMetadata,
        bytes: Vec<u8>,
    ) -> Result<pb::Attachment, SessionError> {
        let mut frames = vec![pb::UploadMediaFrame {
            body: Some(pb::upload_media_frame::Body::Metadata(metadata)),
        }];
        frames.extend(
            bytes
                .chunks(UPLOAD_CHUNK_BYTES)
                .map(|chunk| pb::UploadMediaFrame {
                    body: Some(pb::upload_media_frame::Body::Chunk(chunk.to_vec())),
                }),
        );
        let mut request = tonic::Request::new(tokio_stream::iter(frames));
        request
            .metadata_mut()
            .insert("authorization", self.authorization.clone());
        let attachment = self
            .client
            .clone()
            .upload_media(request)
            .await?
            .into_inner();
        Ok(attachment)
    }
}
