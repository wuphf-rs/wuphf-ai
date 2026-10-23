use std::{
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status};
use uuid::Uuid;
use wuphf_model::{Attachment, AttachmentKind, MediaBlob, MediaId};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::media_transport_server::MediaTransport;
use wuphf_storage::MediaStore;

const DOWNLOAD_CHUNK_BYTES: usize = 64 * 1024;

pub struct GrpcMediaService<S> {
    storage: Arc<S>,
    max_media_bytes: usize,
    orphan_media_retention_ms: i64,
}

impl<S> GrpcMediaService<S> {
    pub fn new(storage: Arc<S>, max_media_bytes: usize, orphan_media_retention: Duration) -> Self {
        Self {
            storage,
            max_media_bytes,
            orphan_media_retention_ms: i64::try_from(orphan_media_retention.as_millis())
                .unwrap_or(i64::MAX),
        }
    }
}

#[tonic::async_trait]
impl<S> MediaTransport for GrpcMediaService<S>
where
    S: MediaStore,
{
    type DownloadMediaStream =
        Pin<Box<dyn Stream<Item = Result<pb::MediaChunk, Status>> + Send + 'static>>;

    async fn upload_media(
        &self,
        request: Request<tonic::Streaming<pb::UploadMediaFrame>>,
    ) -> Result<Response<pb::Attachment>, Status> {
        let mut frames = request.into_inner();
        let metadata = frames
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("media metadata frame required"))?;
        let metadata = match metadata.body {
            Some(pb::upload_media_frame::Body::Metadata(metadata)) => metadata,
            _ => {
                return Err(Status::invalid_argument(
                    "first media frame must be metadata",
                ));
            }
        };
        let mut bytes = Vec::new();
        while let Some(frame) = frames.message().await? {
            let chunk = match frame.body {
                Some(pb::upload_media_frame::Body::Chunk(chunk)) => chunk,
                _ => {
                    return Err(Status::invalid_argument(
                        "media metadata may appear only once",
                    ));
                }
            };
            if bytes.len().saturating_add(chunk.len()) > self.max_media_bytes {
                return Err(Status::resource_exhausted(
                    "media exceeds configured upload limit",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if metadata
            .size_bytes
            .is_some_and(|size| size != bytes.len() as u64)
        {
            return Err(Status::invalid_argument(
                "media size does not match uploaded bytes",
            ));
        }
        let stored_at_ms =
            now_ms().ok_or_else(|| Status::internal("system clock precedes Unix epoch"))?;
        self.storage
            .prune_unreferenced_media_before(
                stored_at_ms.saturating_sub(self.orphan_media_retention_ms),
            )
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        let attachment = Attachment {
            media_id: MediaId::new(),
            kind: attachment_kind(metadata.kind)
                .map_err(|error| Status::invalid_argument(error.to_string()))?,
            file_name: metadata.file_name,
            mime_type: metadata.mime_type,
            size_bytes: Some(bytes.len() as u64),
            animated: metadata.animated,
        };
        self.storage
            .put_media(
                MediaBlob {
                    attachment: attachment.clone(),
                    bytes,
                },
                stored_at_ms,
            )
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        Ok(Response::new(super::grpc::attachment_to_proto(attachment)))
    }

    async fn download_media(
        &self,
        request: Request<pb::DownloadMediaRequest>,
    ) -> Result<Response<Self::DownloadMediaStream>, Status> {
        let media_id = MediaId(
            Uuid::parse_str(&request.into_inner().media_id)
                .map_err(|_| Status::invalid_argument("media_id must be valid UUID"))?,
        );
        let media = self
            .storage
            .media(media_id)
            .await
            .map_err(|error| Status::internal(error.to_string()))?
            .ok_or_else(|| Status::not_found("media not found"))?;
        let (sender, receiver) = mpsc::channel(4);
        tokio::spawn(async move {
            for chunk in media.bytes.chunks(DOWNLOAD_CHUNK_BYTES) {
                if sender
                    .send(Ok(pb::MediaChunk {
                        chunk: chunk.to_vec(),
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }
}

fn now_ms() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
}

#[derive(Debug, Error)]
#[error("{0}")]
struct MetadataError(&'static str);

fn attachment_kind(value: i32) -> Result<AttachmentKind, MetadataError> {
    match pb::attachment::Kind::try_from(value)
        .map_err(|_| MetadataError("invalid attachment kind"))?
    {
        pb::attachment::Kind::Image => Ok(AttachmentKind::Image),
        pb::attachment::Kind::Video => Ok(AttachmentKind::Video),
        pb::attachment::Kind::Audio => Ok(AttachmentKind::Audio),
        pb::attachment::Kind::File => Ok(AttachmentKind::File),
        pb::attachment::Kind::Sticker => Ok(AttachmentKind::Sticker),
        pb::attachment::Kind::Unspecified => Err(MetadataError("attachment kind required")),
    }
}
