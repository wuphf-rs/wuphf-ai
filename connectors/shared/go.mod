module github.com/akshettrj/wuphf/connectors/shared

go 1.25.0

replace github.com/akshettrj/wuphf/proto/go => ../../proto/go

require (
	github.com/akshettrj/wuphf/proto/go v0.0.0-00010101000000-000000000000
	github.com/invopop/jsonschema v0.14.0
	github.com/pelletier/go-toml/v2 v2.2.4
	github.com/propheci/botconf/go v0.6.0
	github.com/rs/zerolog v1.35.1
	google.golang.org/grpc v1.81.1
	google.golang.org/protobuf v1.36.11
)

require (
	github.com/bahlo/generic-list-go v0.2.0 // indirect
	github.com/buger/jsonparser v1.1.2 // indirect
	github.com/mattn/go-colorable v0.1.14 // indirect
	github.com/mattn/go-isatty v0.0.20 // indirect
	github.com/pb33f/ordered-map/v2 v2.3.1 // indirect
	go.yaml.in/yaml/v4 v4.0.0-rc.2 // indirect
	golang.org/x/net v0.51.0 // indirect
	golang.org/x/sys v0.42.0 // indirect
	golang.org/x/text v0.34.0 // indirect
	google.golang.org/genproto/googleapis/rpc v0.0.0-20260226221140-a57be14db171 // indirect
)
