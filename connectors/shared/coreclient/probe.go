package coreclient

import (
	"context"
	"os/exec"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// Prober finds out what this account can do in the chats the core's rules
// use (plan 079): the core asks with a ProbeChats, and the connector answers
// with facts. The core decides what is wrong; a connector only looks.
//
// Answer within the core's probe timeout (20 s). A fact the connector cannot
// learn stays FACT_UNKNOWN, and a chat left out is unknown.
type Prober interface {
	Probe(ctx context.Context, chatIDs []string) []*wuphfpb.ChatFacts
}

// Tools reports the external programs a connector runs, checked now.
type Tools func() []*wuphfpb.ToolStatus

// ToolStatus says whether the external program name is on PATH, naming what
// goes without it ("sticker pack names").
func ToolStatus(name, neededFor string) *wuphfpb.ToolStatus {
	_, err := exec.LookPath(name)
	return &wuphfpb.ToolStatus{Name: name, Available: err == nil, NeededFor: neededFor}
}

// answerProbe asks prober (when set) about the probe's chats and returns the
// ProbeResult frame, with the tools as they are now.
func answerProbe(ctx context.Context, prober Prober, tools Tools, probe *wuphfpb.ProbeChats) *wuphfpb.ClientFrame {
	var chats []*wuphfpb.ChatFacts
	if prober != nil {
		chats = prober.Probe(ctx, probe.GetChatIds())
	}
	return &wuphfpb.ClientFrame{Body: &wuphfpb.ClientFrame_ProbeResult{ProbeResult: &wuphfpb.ProbeResult{
		RequestId: probe.GetRequestId(),
		Chats:     chats,
		Tools:     toolsNow(tools),
	}}}
}

func toolsNow(tools Tools) []*wuphfpb.ToolStatus {
	if tools == nil {
		return nil
	}
	return tools()
}

// Fact turns a known boolean into a Fact.
func Fact(yes bool) wuphfpb.Fact {
	if yes {
		return wuphfpb.Fact_FACT_YES
	}
	return wuphfpb.Fact_FACT_NO
}
