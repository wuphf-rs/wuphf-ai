module github.com/akshettrj/wuphf/connectors/whatsapp

go 1.25.0

replace github.com/akshettrj/wuphf/proto/go => ../../proto/go

replace github.com/akshettrj/wuphf/connectors/mediaconv => ../mediaconv

replace github.com/akshettrj/wuphf/connectors/shared => ../shared

require (
	github.com/akshettrj/wuphf/connectors/mediaconv v0.0.0-00010101000000-000000000000
	github.com/akshettrj/wuphf/connectors/shared v0.0.0-00010101000000-000000000000
	github.com/akshettrj/wuphf/proto/go v0.0.0-00010101000000-000000000000
	github.com/mattn/go-sqlite3 v1.14.44
	github.com/mdp/qrterminal/v3 v3.2.1
	github.com/pelletier/go-toml/v2 v2.3.1
	github.com/rs/zerolog v1.35.1
	github.com/watgbridge/tgsconverter v0.0.0-20240710075117-d1c05581b842
	go.mau.fi/whatsmeow v0.0.0-20260525123251-933deb5f2ee9
	google.golang.org/protobuf v1.36.11
)

require (
	filippo.io/edwards25519 v1.1.0 // indirect
	github.com/Benau/go_rlottie v0.0.0-20210807002906-98c1b2421989 // indirect
	github.com/av-elier/go-decimal-to-rational v0.0.0-20191127152832-89e6aad02ecf // indirect
	github.com/beeper/argo-go v1.1.2 // indirect
	github.com/coder/websocket v1.8.14 // indirect
	github.com/elliotchance/orderedmap/v3 v3.1.0 // indirect
	github.com/google/uuid v1.6.0 // indirect
	github.com/kettek/apng v0.0.0-20191108220231-414630eed80f // indirect
	github.com/mattn/go-colorable v0.1.14 // indirect
	github.com/mattn/go-isatty v0.0.20 // indirect
	github.com/petermattis/goid v0.0.0-20260330135022-df67b199bc81 // indirect
	github.com/vektah/gqlparser/v2 v2.5.27 // indirect
	github.com/watgbridge/webp v0.0.0-20240630070243-84d3a70502bb // indirect
	go.mau.fi/libsignal v0.2.1 // indirect
	go.mau.fi/util v0.9.9 // indirect
	golang.org/x/crypto v0.51.0 // indirect
	golang.org/x/exp v0.0.0-20260508232706-74f9aab9d74a // indirect
	golang.org/x/net v0.54.0 // indirect
	golang.org/x/sync v0.20.0 // indirect
	golang.org/x/sys v0.44.0 // indirect
	golang.org/x/term v0.43.0 // indirect
	golang.org/x/text v0.37.0 // indirect
	google.golang.org/genproto/googleapis/rpc v0.0.0-20260226221140-a57be14db171 // indirect
	google.golang.org/grpc v1.81.1 // indirect
	rsc.io/qr v0.2.0 // indirect
)
