module github.com/akshettrj/wuphf/connectors/shared

go 1.25.0

replace github.com/akshettrj/wuphf/proto/go => ../../proto/go

require (
	github.com/akshettrj/wuphf/proto/go v0.0.0-00010101000000-000000000000
	github.com/rs/zerolog v1.35.1
	google.golang.org/grpc v1.81.1
	google.golang.org/protobuf v1.36.11
)

require (
	github.com/mattn/go-colorable v0.1.14 // indirect
	github.com/mattn/go-isatty v0.0.20 // indirect
	golang.org/x/net v0.51.0 // indirect
	golang.org/x/sys v0.42.0 // indirect
	golang.org/x/text v0.34.0 // indirect
	google.golang.org/genproto/googleapis/rpc v0.0.0-20260226221140-a57be14db171 // indirect
)
