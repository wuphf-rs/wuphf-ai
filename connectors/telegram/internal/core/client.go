package core

import (
	"context"
	"errors"
	"fmt"

	"github.com/akshettrj/wuphf/connectors/telegram/internal/config"
	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

type Sender interface {
	SendMessage(context.Context, *wuphfpb.MessageCreateDispatch) (string, error)
}

type Client struct {
	cfg config.File
}

func New(cfg config.File) Client {
	return Client{cfg: cfg}
}

func (c Client) Run(ctx context.Context, sender Sender, inputs <-chan *wuphfpb.ClientFrame) error {
	connection, err := grpc.NewClient(c.cfg.CoreAddress, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		return fmt.Errorf("connect to core: %w", err)
	}
	defer connection.Close()

	stream, err := wuphfpb.NewConnectorTransportClient(connection).OpenSession(ctx)
	if err != nil {
		return fmt.Errorf("open connector session: %w", err)
	}
	if err := stream.Send(&wuphfpb.ClientFrame{
		Body: &wuphfpb.ClientFrame_Registration{Registration: &wuphfpb.Registration{
			ConnectorId: c.cfg.ConnectorID,
			Platform:    wuphfpb.Platform_PLATFORM_TELEGRAM,
			EndpointIds: c.cfg.Endpoints,
		}},
	}); err != nil {
		return fmt.Errorf("register connector: %w", err)
	}

	frames := make(chan *wuphfpb.ServerFrame)
	receiveErrors := make(chan error, 1)
	go func() {
		for {
			frame, err := stream.Recv()
			if err != nil {
				receiveErrors <- err
				return
			}
			select {
			case frames <- frame:
			case <-ctx.Done():
				return
			}
		}
	}()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case err := <-receiveErrors:
			return fmt.Errorf("receive core dispatch: %w", err)
		case input := <-inputs:
			if err := stream.Send(input); err != nil {
				return fmt.Errorf("send Telegram input: %w", err)
			}
		case frame := <-frames:
			switch body := frame.Body.(type) {
			case *wuphfpb.ServerFrame_MessageCreate:
				messageID, err := sender.SendMessage(ctx, body.MessageCreate)
				if err != nil {
					return fmt.Errorf("send dispatch %s: %w", body.MessageCreate.DispatchId, err)
				}
				if err := stream.Send(&wuphfpb.ClientFrame{
					Body: &wuphfpb.ClientFrame_Acknowledgement{Acknowledgement: &wuphfpb.Acknowledgement{
						DispatchId:        body.MessageCreate.DispatchId,
						PlatformMessageId: messageID,
					}},
				}); err != nil {
					return fmt.Errorf("acknowledge dispatch %s: %w", body.MessageCreate.DispatchId, err)
				}
			case *wuphfpb.ServerFrame_Rejected:
				return fmt.Errorf("core rejected connector operation: %s", body.Rejected.Reason)
			default:
				return errors.New("core returned empty connector frame")
			}
		}
	}
}
