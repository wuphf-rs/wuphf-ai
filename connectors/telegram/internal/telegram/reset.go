package telegramconnector

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/gotd/td/session"
	gotd "github.com/gotd/td/telegram"
	"github.com/gotd/td/tg"
)

// generalTopicID is the fixed id of a forum's built-in "General" topic, which
// cannot be deleted and is preserved by a reset.
const generalTopicID = 1

// forumTopicsPageSize bounds a single messages.getForumTopics page; the listing
// is paginated until fewer than a full page is returned.
const forumTopicsPageSize = 100

// Reset wipes a forum group back to an empty state: every topic other than the
// built-in "General" topic is removed along with all of its messages. It runs a
// one-shot authenticated session (no update handler or core connection) and,
// unless confirm is set, prompts for confirmation before deleting anything.
func (c Connector) Reset(ctx context.Context, targetChatID string, confirm bool) error {
	peers := NewPeerCache()
	client := gotd.NewClient(c.credentials.AppID, c.credentials.AppHash, gotd.Options{
		SessionStorage: &session.FileStorage{Path: c.cfg.SessionFile},
	})
	return client.Run(ctx, func(ctx context.Context) error {
		if err := c.authenticate(ctx, client); err != nil {
			return err
		}
		// Addressing the target needs its access hash. Bots only learn hashes
		// from inbound updates and cannot list dialogs, so reset requires a user
		// session. Check the stored session's identity rather than the
		// configured credentials, since a reused session_file may still be
		// authorized as a bot from an earlier run.
		self, err := client.Self(ctx)
		if err != nil {
			return fmt.Errorf("resolve Telegram session identity: %w", err)
		}
		if self.Bot {
			return fmt.Errorf("session file %q is authorized as a bot; reset needs a user session: "+
				"point session_file at a fresh path and log in with your phone", c.cfg.SessionFile)
		}
		if err := primePeers(ctx, client.API(), peers); err != nil {
			return err
		}
		return resetForum(ctx, client.API(), peers, targetChatID, confirm)
	})
}

func resetForum(ctx context.Context, api *tg.Client, peers *PeerCache, targetChatID string, confirm bool) error {
	peer, err := peers.InputPeer(targetChatID)
	if err != nil {
		return err
	}
	topicIDs, err := collectForumTopics(ctx, api, peer)
	if err != nil {
		return err
	}
	targets := make([]int, 0, len(topicIDs))
	for _, id := range topicIDs {
		if id != generalTopicID {
			targets = append(targets, id)
		}
	}
	if len(targets) == 0 {
		fmt.Printf("no deletable topics in %s (General is preserved)\n", targetChatID)
		return nil
	}
	if !confirm {
		fmt.Fprintf(os.Stderr,
			"About to delete %d topic(s) and all their messages in %s (General is preserved).\nType 'yes' to continue: ",
			len(targets), targetChatID)
		line, err := bufio.NewReader(os.Stdin).ReadString('\n')
		if err != nil {
			return err
		}
		if strings.TrimSpace(line) != "yes" {
			return errors.New("reset aborted")
		}
	}
	for _, topicID := range targets {
		if err := deleteTopicHistory(ctx, api, peer, topicID); err != nil {
			return fmt.Errorf("delete topic %d: %w", topicID, err)
		}
		fmt.Printf("deleted topic %d and its messages\n", topicID)
	}
	fmt.Printf("reset complete: removed %d topic(s) from %s\n", len(targets), targetChatID)
	return nil
}

// collectForumTopics paginates messages.getForumTopics and returns every topic
// id in the forum. Pagination keys off the last topic of each page, following
// the offset trio (date, top message id, topic id) Telegram expects.
func collectForumTopics(ctx context.Context, api *tg.Client, peer tg.InputPeerClass) ([]int, error) {
	var (
		ids                                []int
		offsetDate, offsetID, offsetTopic int
	)
	for {
		resp, err := api.MessagesGetForumTopics(ctx, &tg.MessagesGetForumTopicsRequest{
			Peer:        peer,
			Limit:       forumTopicsPageSize,
			OffsetDate:  offsetDate,
			OffsetID:    offsetID,
			OffsetTopic: offsetTopic,
		})
		if err != nil {
			return nil, fmt.Errorf("list Telegram forum topics: %w", err)
		}
		if len(resp.Topics) == 0 {
			break
		}
		for _, candidate := range resp.Topics {
			topic, ok := candidate.(*tg.ForumTopic)
			if !ok {
				continue
			}
			ids = append(ids, topic.ID)
			offsetDate = topic.Date
			offsetID = topic.TopMessage
			offsetTopic = topic.ID
		}
		if len(resp.Topics) < forumTopicsPageSize {
			break
		}
	}
	return ids, nil
}

// deleteTopicHistory clears a single topic's entire message history. Telegram
// deletes history in capped batches, signalling more remains via a non-zero
// offset, so the call is repeated until the offset reaches zero.
func deleteTopicHistory(ctx context.Context, api *tg.Client, peer tg.InputPeerClass, topicID int) error {
	for {
		affected, err := api.MessagesDeleteTopicHistory(ctx, &tg.MessagesDeleteTopicHistoryRequest{
			Peer:     peer,
			TopMsgID: topicID,
		})
		if err != nil {
			return err
		}
		if affected.Offset == 0 {
			return nil
		}
	}
}
