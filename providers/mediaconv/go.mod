module github.com/akshettrj/wuphf/providers/mediaconv

go 1.25.0

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
	github.com/av-elier/go-decimal-to-rational v0.0.0-20191127152832-89e6aad02ecf // indirect
	github.com/davecgh/go-spew v1.1.1 // indirect
	github.com/kettek/apng v0.0.0-20191108220231-414630eed80f // indirect
	github.com/mattn/go-colorable v0.1.14 // indirect
	github.com/mattn/go-isatty v0.0.20 // indirect
	github.com/pmezard/go-difflib v1.0.0 // indirect
	github.com/rs/zerolog v1.35.1 // indirect
	github.com/watgbridge/webp v0.0.0-20240630070243-84d3a70502bb // indirect
	golang.org/x/net v0.51.0 // indirect
	golang.org/x/sys v0.42.0 // indirect
	golang.org/x/text v0.34.0 // indirect
	google.golang.org/genproto/googleapis/rpc v0.0.0-20260226221140-a57be14db171 // indirect
	google.golang.org/grpc v1.81.1 // indirect
	google.golang.org/protobuf v1.36.11 // indirect
	gopkg.in/yaml.v3 v3.0.1 // indirect
)
