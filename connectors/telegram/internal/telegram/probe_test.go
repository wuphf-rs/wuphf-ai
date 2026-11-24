package telegramconnector

import (
	"testing"

	"github.com/gotd/td/tg"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

func channel(configure func(*tg.Channel)) *wuphfpb.ChatFacts {
	chat := &tg.Channel{ID: 100, Megagroup: true}
	configure(chat)
	facts := &wuphfpb.ChatFacts{}
	channelFacts(facts, chat)
	return facts
}

func TestAGroupWithoutTopicsIsNoForum(t *testing.T) {
	facts := channel(func(*tg.Channel) {})
	if facts.GetForum() != wuphfpb.Fact_FACT_NO || facts.GetCanCreateTopics() != wuphfpb.Fact_FACT_NO {
		t.Fatalf("facts = %v", facts)
	}
	if facts.GetCanSend() != wuphfpb.Fact_FACT_YES || facts.GetMember() != wuphfpb.Fact_FACT_YES {
		t.Fatalf("an ordinary member of an open group can post: %v", facts)
	}
}

func TestCreatingTopicsNeedsTheRightOrOpenDefaults(t *testing.T) {
	closed := channel(func(chat *tg.Channel) {
		chat.Forum = true
		chat.SetDefaultBannedRights(tg.ChatBannedRights{ManageTopics: true})
	})
	if closed.GetCanCreateTopics() != wuphfpb.Fact_FACT_NO {
		t.Fatalf("a member in a forum closed to topics: %v", closed)
	}
	admin := channel(func(chat *tg.Channel) {
		chat.Forum = true
		chat.SetDefaultBannedRights(tg.ChatBannedRights{ManageTopics: true})
		chat.SetAdminRights(tg.ChatAdminRights{ManageTopics: true})
	})
	if admin.GetCanCreateTopics() != wuphfpb.Fact_FACT_YES {
		t.Fatalf("an admin with Manage Topics: %v", admin)
	}
	open := channel(func(chat *tg.Channel) { chat.Forum = true })
	if open.GetCanCreateTopics() != wuphfpb.Fact_FACT_YES {
		t.Fatalf("anyone may create topics: %v", open)
	}
}

func TestABroadcastChannelNeedsThePostRight(t *testing.T) {
	member := channel(func(chat *tg.Channel) { chat.Megagroup, chat.Broadcast = false, true })
	if member.GetCanSend() != wuphfpb.Fact_FACT_NO {
		t.Fatalf("a subscriber cannot post: %v", member)
	}
	poster := channel(func(chat *tg.Channel) {
		chat.Megagroup, chat.Broadcast = false, true
		chat.SetAdminRights(tg.ChatAdminRights{PostMessages: true})
	})
	if poster.GetCanSend() != wuphfpb.Fact_FACT_YES {
		t.Fatalf("an admin with Post Messages can: %v", poster)
	}
}

func TestALeftGroupIsNotAMember(t *testing.T) {
	facts := channel(func(chat *tg.Channel) { chat.Left = true })
	if facts.GetMember() != wuphfpb.Fact_FACT_NO || facts.GetCanSend() != wuphfpb.Fact_FACT_NO {
		t.Fatalf("facts = %v", facts)
	}
}
