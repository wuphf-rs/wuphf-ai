module github.com/akshettrj/wuphf/providers/mediaconv

go 1.26.0

replace github.com/akshettrj/wuphf/proto/go => ../../proto/go

replace github.com/akshettrj/wuphf/connectors/shared => ../../connectors/shared

require (
	github.com/akshettrj/wuphf/connectors/shared v0.0.0-00010101000000-000000000000
	github.com/akshettrj/wuphf/proto/go v0.0.0-00010101000000-000000000000
	github.com/pelletier/go-toml/v2 v2.4.3
	github.com/watgbridge/tgsconverter v0.0.0-20240710075117-d1c05581b842
)

require (
	github.com/Benau/go_rlottie v0.0.0-20210807002906-98c1b2421989 // indirect
	github.com/av-elier/go-decimal-to-rational v0.0.0-20250603203441-f39a07f43ff3 // indirect
	github.com/kettek/apng v0.0.0-20250827064933-2bb5f5fcf253 // indirect
	github.com/mattn/go-colorable v0.1.16 // indirect
	github.com/mattn/go-isatty v0.0.24 // indirect
	github.com/rs/zerolog v1.35.1 // indirect
	github.com/watgbridge/webp v0.0.0-20240709143015-99fb5316f772 // indirect
	golang.org/x/net v0.62.0 // indirect
	golang.org/x/sys v0.50.0 // indirect
	golang.org/x/text v0.44.0 // indirect
	google.golang.org/genproto/googleapis/rpc v0.0.0-20261110000943-09e21b237f2c // indirect
	google.golang.org/grpc v1.85.0 // indirect
	google.golang.org/protobuf v1.36.12 // indirect
)
