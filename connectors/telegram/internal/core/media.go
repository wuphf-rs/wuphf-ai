package core

import (
	"context"
	"fmt"
	"io"

	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

const uploadChunkBytes = 64 * 1024

type MediaClient struct {
	connection *grpc.ClientConn
	client     wuphfpb.MediaTransportClient
}

func DialMedia(address string) (*MediaClient, error) {
	connection, err := grpc.NewClient(address, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		return nil, fmt.Errorf("connect to core media service: %w", err)
	}
	return &MediaClient{
		connection: connection,
		client:     wuphfpb.NewMediaTransportClient(connection),
	}, nil
}

func (c *MediaClient) Close() error {
	return c.connection.Close()
}

func (c *MediaClient) Upload(
	ctx context.Context,
	metadata *wuphfpb.MediaMetadata,
	bytes []byte,
) (*wuphfpb.Attachment, error) {
	stream, err := c.client.UploadMedia(ctx)
	if err != nil {
		return nil, fmt.Errorf("open media upload: %w", err)
	}
	if err := stream.Send(&wuphfpb.UploadMediaFrame{
		Body: &wuphfpb.UploadMediaFrame_Metadata{Metadata: metadata},
	}); err != nil {
		return nil, fmt.Errorf("send media metadata: %w", err)
	}
	for offset := 0; offset < len(bytes); offset += uploadChunkBytes {
		end := min(offset+uploadChunkBytes, len(bytes))
		if err := stream.Send(&wuphfpb.UploadMediaFrame{
			Body: &wuphfpb.UploadMediaFrame_Chunk{Chunk: bytes[offset:end]},
		}); err != nil {
			return nil, fmt.Errorf("send media bytes: %w", err)
		}
	}
	attachment, err := stream.CloseAndRecv()
	if err != nil {
		return nil, fmt.Errorf("complete media upload: %w", err)
	}
	return attachment, nil
}

func (c *MediaClient) Download(ctx context.Context, mediaID string) ([]byte, error) {
	stream, err := c.client.DownloadMedia(ctx, &wuphfpb.DownloadMediaRequest{MediaId: mediaID})
	if err != nil {
		return nil, fmt.Errorf("open media download: %w", err)
	}
	var bytes []byte
	for {
		chunk, err := stream.Recv()
		if err == io.EOF {
			return bytes, nil
		}
		if err != nil {
			return nil, fmt.Errorf("receive media bytes: %w", err)
		}
		bytes = append(bytes, chunk.Chunk...)
	}
}
