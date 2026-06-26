package whatsappconnector

import (
	"context"
	"sync"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
)

// identityResolver collapses WhatsApp's interchangeable address forms to one
// stable identity per peer. WhatsApp refers to the same user by a phone-number
// JID (`<n>@s.whatsapp.net`), a hidden LID (`<n>@lid`), and per-device AD-JIDs
// (a device suffix on either), switching between them from one message to the
// next. The core keys an endpoint, and therefore a mirror thread, on the raw
// chat id; left unmapped each form mints its own endpoint and one chat is split
// across several mirror threads.
//
// The resolver is the single place that knows how to canonicalize a WhatsApp
// address, so callers never branch on the address space. Resolution is best
// effort: when no mapping is known the input (device stripped) is returned
// unchanged, and a later sighting can still migrate it once the mapping lands.
type identityResolver interface {
	// Canonical maps any chat/peer/sender JID to its stable bridge identity:
	// the device is always stripped, and a phone JID is promoted to its LID
	// when one is known.
	Canonical(ctx context.Context, jid types.JID) types.JID
}

// whatsmeowIdentity resolves canonical identities through the whatsmeow LID
// store. Phone-to-LID mappings are stable for the life of a contact, so they are
// cached for the session; a lookup miss is not cached, as the mapping may be
// learned later and must then take effect.
type whatsmeowIdentity struct {
	client *whatsmeow.Client
	mu     sync.RWMutex
	lids   map[string]types.JID
}

func newIdentityResolver(client *whatsmeow.Client) *whatsmeowIdentity {
	return &whatsmeowIdentity{client: client, lids: make(map[string]types.JID)}
}

func (r *whatsmeowIdentity) Canonical(ctx context.Context, jid types.JID) types.JID {
	jid = jid.ToNonAD()
	// Only phone-number users have a LID counterpart; groups, newsletters, and
	// already-LID addresses are canonical as-is.
	if jid.Server != types.DefaultUserServer {
		return jid
	}
	key := jid.String()
	r.mu.RLock()
	cached, ok := r.lids[key]
	r.mu.RUnlock()
	if ok {
		return cached
	}
	lid, err := r.client.Store.LIDs.GetLIDForPN(ctx, jid)
	if err != nil || lid.IsEmpty() {
		return jid
	}
	r.mu.Lock()
	r.lids[key] = lid
	r.mu.Unlock()
	return lid
}

// migrationGate suppresses re-reporting a chat identity migration the core has
// already been told about. migrate_identity is idempotent, but a connector emits
// the same phone-to-LID pair on every message of a long-lived chat; deduping
// keeps the migration stream (and its logs) to one entry per address change per
// session.
type migrationGate struct {
	mu   sync.Mutex
	seen map[string]bool
}

func newMigrationGate() *migrationGate {
	return &migrationGate{seen: make(map[string]bool)}
}

// unseen returns the migrations not yet reported this session, marking them
// reported. A nil gate disables deduping (passes everything through), so a
// resolver-less normalizer still works in tests.
func (g *migrationGate) unseen(migrations []*wuphfpb.IdentityMigrated) []*wuphfpb.IdentityMigrated {
	if g == nil || len(migrations) == 0 {
		return migrations
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	out := migrations[:0:0]
	for _, migration := range migrations {
		key := migration.GetOldChatId() + "\x00" + migration.GetNewChatId()
		if g.seen[key] {
			continue
		}
		g.seen[key] = true
		out = append(out, migration)
	}
	return out
}
