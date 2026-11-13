module github.com/akshettrj/wuphf/connectors/shared

go 1.26.0

replace github.com/akshettrj/wuphf/proto/go => ../../proto/go

require (
	github.com/akshettrj/wuphf/proto/go v0.0.0-00010101000000-000000000000
	github.com/invopop/jsonschema v0.14.0
	github.com/pelletier/go-toml/v2 v2.4.3
	github.com/propheci/botconf/go v0.10.0
	github.com/rs/zerolog v1.35.1
	google.golang.org/grpc v1.85.0
	google.golang.org/protobuf v1.36.12
)

require (
	github.com/bahlo/generic-list-go v0.2.0 // indirect
	github.com/buger/jsonparser v1.6.1 // indirect
	github.com/mattn/go-colorable v0.1.16 // indirect
	github.com/mattn/go-isatty v0.0.24 // indirect
	github.com/pb33f/go-yaml v0.1.1 // indirect
	github.com/pb33f/ordered-map/v2 v2.3.2 // indirect
	golang.org/x/net v0.62.0 // indirect
	golang.org/x/sys v0.50.0 // indirect
	golang.org/x/text v0.44.0 // indirect
	google.golang.org/genproto/googleapis/rpc v0.0.0-20261110000943-09e21b237f2c // indirect
)
