module github.com/akshettrj/wuphf/connectors/whatsapp

go 1.26.0

replace github.com/akshettrj/wuphf/proto/go => ../../proto/go

replace github.com/akshettrj/wuphf/connectors/shared => ../shared

require (
	github.com/akshettrj/wuphf/connectors/shared v0.0.0-00010101000000-000000000000
	github.com/akshettrj/wuphf/proto/go v0.0.0-00010101000000-000000000000
	github.com/mattn/go-sqlite3 v1.14.53
	github.com/mdp/qrterminal/v3 v3.2.1
	github.com/pelletier/go-toml/v2 v2.4.3
	github.com/rs/zerolog v1.35.1
	go.mau.fi/whatsmeow v0.0.0-20261113153213-1956f9683795
	google.golang.org/protobuf v1.36.12
)

require (
	filippo.io/edwards25519 v1.2.0 // indirect
	github.com/bahlo/generic-list-go v0.2.0 // indirect
	github.com/beeper/argo-go v1.1.2 // indirect
	github.com/buger/jsonparser v1.6.1 // indirect
	github.com/coder/websocket v1.8.15 // indirect
	github.com/elliotchance/orderedmap/v3 v3.1.1 // indirect
	github.com/google/uuid v1.6.0 // indirect
	github.com/invopop/jsonschema v0.14.0 // indirect
	github.com/mattn/go-colorable v0.1.16 // indirect
	github.com/mattn/go-isatty v0.0.24 // indirect
	github.com/pb33f/go-yaml v0.1.1 // indirect
	github.com/pb33f/ordered-map/v2 v2.3.2 // indirect
	github.com/petermattis/goid v0.0.0-20261109124353-7cfa343ad0ab // indirect
	github.com/propheci/botconf/go v0.10.0 // indirect
	github.com/vektah/gqlparser/v2 v2.5.64 // indirect
	go.mau.fi/libsignal v0.2.3 // indirect
	go.mau.fi/util v0.11.0 // indirect
	golang.org/x/crypto v0.59.0 // indirect
	golang.org/x/exp v0.0.0-20261113113407-e3970619a1a4 // indirect
	golang.org/x/net v0.62.0 // indirect
	golang.org/x/sync v0.25.0 // indirect
	golang.org/x/sys v0.50.0 // indirect
	golang.org/x/term v0.48.0 // indirect
	golang.org/x/text v0.44.0 // indirect
	google.golang.org/genproto/googleapis/rpc v0.0.0-20261110000943-09e21b237f2c // indirect
	google.golang.org/grpc v1.85.0 // indirect
	rsc.io/qr v0.2.0 // indirect
)
