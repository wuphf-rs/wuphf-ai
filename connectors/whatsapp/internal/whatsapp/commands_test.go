package whatsappconnector

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"github.com/wuphf-rs/wuphf-ai/connectors/whatsapp/internal/config"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow/appstate"
)

// fetchRecorder records each collection fetched, waiting on release first
// when set.
type fetchRecorder struct {
	mu      sync.Mutex
	fetched []appstate.WAPatchName
	full    []bool
	release chan struct{}
	done    chan struct{}
}

func (f *fetchRecorder) FetchAppState(_ context.Context, name appstate.WAPatchName, fullSync, _ bool) error {
	if f.release != nil {
		<-f.release
	}
	f.mu.Lock()
	f.fetched = append(f.fetched, name)
	f.full = append(f.full, fullSync)
	count := len(f.fetched)
	f.mu.Unlock()
	if count == len(appstate.AllPatchNames) && f.done != nil {
		close(f.done)
	}
	return nil
}

func testCommands(client appStateFetcher, method string) (*whatsappCommands, *linking) {
	inputs := make(chan *wuphfpb.ClientFrame, 4)
	linking := newLinking(inputs, &uploadBroker{}, method, zerolog.Nop())
	fetch := func() appStateFetcher { return client }
	return newCommands(context.Background(), fetch, linking, zerolog.Nop()), linking
}

func TestBothCommandsAreDeclared(t *testing.T) {
	t.Parallel()
	commands, _ := testCommands(nil, config.LoginMethodQR)
	declared := commands.Declarations()
	if len(declared) != 2 || declared[0].GetId() != commandResyncAppState || declared[1].GetId() != commandFreshLinkCode {
		t.Fatalf("declared %v", declared)
	}
}

func TestAResyncFetchesEveryCollectionWhole(t *testing.T) {
	t.Parallel()
	fetcher := &fetchRecorder{done: make(chan struct{})}
	commands, _ := testCommands(fetcher, config.LoginMethodQR)
	if _, err := commands.Run(context.Background(), commandResyncAppState); err != nil {
		t.Fatal(err)
	}
	select {
	case <-fetcher.done:
	case <-time.After(5 * time.Second):
		t.Fatal("not every collection was fetched")
	}
	fetcher.mu.Lock()
	defer fetcher.mu.Unlock()
	for i, name := range appstate.AllPatchNames {
		if fetcher.fetched[i] != name || !fetcher.full[i] {
			t.Fatalf("fetch %d = %s (full %v), want %s in full", i, fetcher.fetched[i], fetcher.full[i], name)
		}
	}
}

func TestOneResyncRunsAtATime(t *testing.T) {
	t.Parallel()
	fetcher := &fetchRecorder{release: make(chan struct{}), done: make(chan struct{})}
	commands, _ := testCommands(fetcher, config.LoginMethodQR)
	if _, err := commands.Run(context.Background(), commandResyncAppState); err != nil {
		t.Fatal(err)
	}
	if _, err := commands.Run(context.Background(), commandResyncAppState); err == nil {
		t.Fatal("a second resync started while the first runs")
	}
	close(fetcher.release)
	<-fetcher.done
}

func TestAnUnlinkedSessionCannotResync(t *testing.T) {
	t.Parallel()
	commands, _ := testCommands(nil, config.LoginMethodQR)
	if _, err := commands.Run(context.Background(), commandResyncAppState); err == nil {
		t.Fatal("an unlinked session resynced")
	}
}

func TestAFreshCodeIsAskedForAtOnce(t *testing.T) {
	t.Parallel()
	commands, linking := testCommands(nil, config.LoginMethodPairCode)
	if _, ok := linking.PairCodeAsk(); !ok {
		t.Fatal("no first ask")
	}
	if quiet, _ := linking.PairCodeAsk(); quiet.tell || linking.FreshPending() {
		t.Fatal("inside the gap a code is not told")
	}
	if _, err := commands.Run(context.Background(), commandFreshLinkCode); err != nil {
		t.Fatal(err)
	}
	if !linking.FreshPending() {
		t.Fatal("the round does not know to ask again")
	}
	ask, _ := linking.PairCodeAsk()
	if !ask.tell || !ask.promptPhone || linking.FreshPending() {
		t.Fatalf("ask = %+v, want told and prompted once", ask)
	}
	if again, _ := linking.PairCodeAsk(); again.tell {
		t.Fatal("the fresh code was told twice")
	}
}

func TestALinkedSessionNeedsNoCode(t *testing.T) {
	t.Parallel()
	commands, linking := testCommands(&fetchRecorder{}, config.LoginMethodPairCode)
	message, err := commands.Run(context.Background(), commandFreshLinkCode)
	if err != nil || message != "WhatsApp is already linked" || linking.FreshPending() {
		t.Fatalf("Run() = (%q, %v)", message, err)
	}
}
