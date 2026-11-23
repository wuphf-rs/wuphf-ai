module github.com/akshettrj/wuphf/connectors/instagram

go 1.26.0

replace github.com/akshettrj/wuphf/proto/go => ../../proto/go

replace github.com/akshettrj/wuphf/connectors/shared => ../shared

require (
	github.com/akshettrj/wuphf/connectors/shared v0.0.0-00010101000000-000000000000
	github.com/akshettrj/wuphf/proto/go v0.0.0-00010101000000-000000000000
	github.com/pelletier/go-toml/v2 v2.4.3
	github.com/rs/zerolog v1.35.1
	go.mau.fi/mautrix-meta v0.2610.0
	go.mau.fi/util v0.11.0
)

require (
	filippo.io/edwards25519 v1.2.0 // indirect
	github.com/andybalholm/brotli v1.3.0 // indirect
	github.com/apache/thrift v0.25.0 // indirect
	github.com/bahlo/generic-list-go v0.2.0 // indirect
	github.com/beeper/poly1305 v0.0.0-20250815183548-d4eede7bbf3c // indirect
	github.com/buger/jsonparser v1.6.1 // indirect
	github.com/coder/websocket v1.8.15 // indirect
	github.com/coreos/go-systemd/v22 v22.7.0 // indirect
	github.com/google/flatbuffers v25.12.19+incompatible // indirect
	github.com/google/go-querystring v1.2.0 // indirect
	github.com/google/uuid v1.6.0 // indirect
	github.com/icholy/digest v1.2.0 // indirect
	github.com/imroc/req/v3 v3.61.0 // indirect
	github.com/invopop/jsonschema v0.14.0 // indirect
	github.com/klauspost/compress v1.20.4 // indirect
	github.com/mattn/go-colorable v0.1.16 // indirect
	github.com/mattn/go-isatty v0.0.24 // indirect
	github.com/pb33f/go-yaml v0.1.1 // indirect
	github.com/pb33f/ordered-map/v2 v2.3.2 // indirect
	github.com/petermattis/goid v0.0.0-20261109124353-7cfa343ad0ab // indirect
	github.com/propheci/botconf/go v0.13.2 // indirect
	github.com/quic-go/qpack v0.6.0 // indirect
	github.com/quic-go/quic-go v0.61.0 // indirect
	github.com/refraction-networking/utls v1.8.2 // indirect
	github.com/rs/xid v1.6.0 // indirect
	github.com/tidwall/gjson v1.20.2 // indirect
	github.com/tidwall/match v1.2.0 // indirect
	github.com/tidwall/pretty v1.3.0 // indirect
	github.com/tidwall/sjson v1.2.9 // indirect
	github.com/yuin/goldmark v1.8.7 // indirect
	go.mau.fi/zeroconfig v0.2.0 // indirect
	golang.org/x/crypto v0.59.0 // indirect
	golang.org/x/exp v0.0.0-20261113113407-e3970619a1a4 // indirect
	golang.org/x/net v0.62.0 // indirect
	golang.org/x/sys v0.50.0 // indirect
	golang.org/x/text v0.44.0 // indirect
	google.golang.org/genproto/googleapis/rpc v0.0.0-20261110000943-09e21b237f2c // indirect
	google.golang.org/grpc v1.85.0 // indirect
	google.golang.org/protobuf v1.36.12 // indirect
	gopkg.in/natefinch/lumberjack.v2 v2.2.1 // indirect
	gopkg.in/yaml.v3 v3.0.1 // indirect
	maunium.net/go/mautrix v0.32.0 // indirect
)

// mautrix-meta pins a Beeper fork of req: upstream req v3.56 predates the qpack
// v0.6 API that mautrix-meta's quic-go needs, and later upstream releases drop
// req.WithTransportOverride, which messagix calls. mautrix-meta declares this as
// a `replace`, which Go applies only in the main module — so every consumer of
// messagix has to repeat it. Keep this pinned to whatever mautrix-meta's own
// go.mod replaces req with.
replace github.com/imroc/req/v3 => github.com/beeper/req/v3 v3.0.0-20260925135712-b49593750e56
