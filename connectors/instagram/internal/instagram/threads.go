package instagramconnector

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"strconv"
	"sync"

	"go.mau.fi/mautrix-meta/pkg/instameow"
	"go.mau.fi/mautrix-meta/pkg/instameow/slidetypes"
)

// ThreadFetcher is the subset of the instameow client the directory uses to
// fill a cache miss. *instameow.Client satisfies it.
type ThreadFetcher interface {
	GetThread(context.Context, *slidetypes.GetThreadInfoRequest) (*slidetypes.ThreadInfoResponse, error)
	FetchThreadID(ctx context.Context, threadFBID int64) (*instameow.ThreadIGIDs, error)
}

// ErrThreadUnknown is returned when a thread cannot be resolved even after
// asking Instagram, so retrying will not help.
var ErrThreadUnknown = errors.New("instagram thread unknown")

// ThreadDirectory translates between Instagram's three thread ids and remembers
// what a chat title is built from. Instagram addresses one thread three ways:
//
//   - the FB thread key (int64), stable and the connector's chat id, so the
//     core's routing rules and copy mappings survive the protocol change;
//   - the short IG id, which deltas carry and text sends, edits and
//     reactions address;
//   - the long IG id, which unsends address.
//
// Messages carry neither thread names nor the peer's name, while the core only
// learns a source title through MessageCreateEvent.chat_title, so the directory
// also keeps each thread's display name ready for every create event.
type ThreadDirectory struct {
	fetcher   ThreadFetcher
	ownUserID int64

	mu      sync.Mutex
	byShort map[string]int64
	threads map[int64]*threadEntry
	users   map[int64]string
}

type threadEntry struct {
	shortID string
	longID  string
	title   string
	isGroup bool
}

func NewThreadDirectory(fetcher ThreadFetcher, ownUserID int64) *ThreadDirectory {
	return &ThreadDirectory{
		fetcher:   fetcher,
		ownUserID: ownUserID,
		byShort:   map[string]int64{},
		threads:   map[int64]*threadEntry{},
		users:     map[int64]string{},
	}
}

// RecordMailbox primes the directory from the inbox listing so the threads
// people are active in resolve without a round trip.
func (d *ThreadDirectory) RecordMailbox(mailbox *slidetypes.Mailbox) {
	if mailbox == nil {
		return
	}
	for _, edge := range mailbox.ThreadsByFolder.Edges {
		d.RecordThread(edge.Node.AsIGDirectThread)
	}
	for _, pinned := range mailbox.PinnedThreadsV2 {
		d.RecordThread(pinned.AsIGDirectThread)
	}
}

// RecordThread stores everything a full thread object says about ids, title
// and participants.
func (d *ThreadDirectory) RecordThread(info *slidetypes.ThreadInfo) {
	if info == nil || info.ThreadKey == 0 {
		return
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	entry := d.entryLocked(info.ThreadKey)
	entry.isGroup = info.IsGroup
	if info.ID != "" {
		entry.shortID = info.ID
		d.byShort[info.ID] = info.ThreadKey
	}
	if info.ThreadID != "" {
		entry.longID = info.ThreadID
	}
	for _, user := range info.Users {
		d.recordUserLocked(user)
	}
	entry.title = cmp.Or(info.ThreadTitle, d.peerNameLocked(info), entry.title)
}

// RecordTitle applies a rename (or a member change that carries the current
// name) to a thread already known by its short id.
func (d *ThreadDirectory) RecordTitle(shortID, title string) {
	if title == "" {
		return
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	if threadKey, ok := d.byShort[shortID]; ok {
		d.threads[threadKey].title = title
	}
}

// RecordUser remembers a user's display name from any user object an event
// carries.
func (d *ThreadDirectory) RecordUser(user *slidetypes.User) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.recordUserLocked(user)
}

// ResolveShort maps a delta's short thread id to the thread key, asking
// Instagram for the thread when it has not been seen yet.
func (d *ThreadDirectory) ResolveShort(ctx context.Context, shortID string) (int64, error) {
	if shortID == "" {
		return 0, fmt.Errorf("%w: empty thread id", ErrThreadUnknown)
	}
	d.mu.Lock()
	threadKey, ok := d.byShort[shortID]
	d.mu.Unlock()
	if ok {
		return threadKey, nil
	}
	resp, err := d.fetcher.GetThread(ctx, slidetypes.MakeGetThreadInfoRequest(shortID))
	if err != nil {
		return 0, fmt.Errorf("fetch Instagram thread %s: %w", shortID, err)
	}
	info := resp.ThreadInfo.AsIGDirectThread
	if info == nil || info.ThreadKey == 0 {
		return 0, fmt.Errorf("%w: %s", ErrThreadUnknown, shortID)
	}
	// The listing may omit the short id; the one asked for is authoritative.
	if info.ID == "" {
		info.ID = shortID
	}
	d.RecordThread(info)
	return info.ThreadKey, nil
}

// ShortID returns the short id a send, edit or reaction addresses.
func (d *ThreadDirectory) ShortID(ctx context.Context, threadKey int64) (string, error) {
	entry, err := d.addressable(ctx, threadKey)
	if err != nil {
		return "", err
	}
	return entry.shortID, nil
}

// LongID returns the long id an unsend addresses.
func (d *ThreadDirectory) LongID(ctx context.Context, threadKey int64) (string, error) {
	entry, err := d.addressable(ctx, threadKey)
	if err != nil {
		return "", err
	}
	return entry.longID, nil
}

// addressable returns a snapshot of the thread's ids, fetching both string ids
// when either is missing (the mailbox listing does not always carry the long
// id).
func (d *ThreadDirectory) addressable(ctx context.Context, threadKey int64) (threadEntry, error) {
	d.mu.Lock()
	if entry, ok := d.threads[threadKey]; ok && entry.shortID != "" && entry.longID != "" {
		snapshot := *entry
		d.mu.Unlock()
		return snapshot, nil
	}
	d.mu.Unlock()
	ids, err := d.fetcher.FetchThreadID(ctx, threadKey)
	if errors.Is(err, instameow.ErrThreadNotFound) {
		return threadEntry{}, fmt.Errorf("%w: %d: %w", ErrThreadUnknown, threadKey, err)
	} else if err != nil {
		return threadEntry{}, fmt.Errorf("fetch Instagram thread ids for %d: %w", threadKey, err)
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	entry := d.entryLocked(threadKey)
	entry.shortID, entry.longID = ids.ShortID, ids.LongID
	d.byShort[ids.ShortID] = threadKey
	return *entry, nil
}

// ChatTitle is the thread's display name tagged with its kind ([G] group, [P]
// one-to-one) so the mirror topic shows which it is. Nil when unknown.
func (d *ThreadDirectory) ChatTitle(threadKey int64) *string {
	d.mu.Lock()
	defer d.mu.Unlock()
	entry, ok := d.threads[threadKey]
	if !ok || entry.title == "" {
		return nil
	}
	tag := "[P] "
	if entry.isGroup {
		tag = "[G] "
	}
	title := tag + entry.title
	return &title
}

// UserName returns the display name recorded for a user, or "" when unknown.
func (d *ThreadDirectory) UserName(userID int64) string {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.users[userID]
}

func (d *ThreadDirectory) entryLocked(threadKey int64) *threadEntry {
	entry, ok := d.threads[threadKey]
	if !ok {
		entry = &threadEntry{}
		d.threads[threadKey] = entry
	}
	return entry
}

func (d *ThreadDirectory) recordUserLocked(user *slidetypes.User) {
	if user == nil || user.InteropMessagingUserFBID == 0 {
		return
	}
	if name := cmp.Or(user.FullName, user.Username); name != "" {
		d.users[user.InteropMessagingUserFBID] = name
	}
}

// peerNameLocked names a one-to-one thread after the participant who is not
// the account owner; Instagram leaves such threads untitled.
func (d *ThreadDirectory) peerNameLocked(info *slidetypes.ThreadInfo) string {
	if info.IsGroup {
		return ""
	}
	for _, user := range info.Users {
		if user != nil && user.InteropMessagingUserFBID != d.ownUserID {
			if name := d.users[user.InteropMessagingUserFBID]; name != "" {
				return name
			}
		}
	}
	return ""
}

func formatThreadKey(threadKey int64) string {
	return strconv.FormatInt(threadKey, 10)
}
