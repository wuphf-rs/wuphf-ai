package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"

	"github.com/akshettrj/wuphf/connectors/instagram/internal/config"
	instagramconnector "github.com/akshettrj/wuphf/connectors/instagram/internal/instagram"
)

func main() {
	configPath := flag.String("config", "config.example.toml", "path to connector TOML configuration")
	flag.Parse()
	cfg, err := config.Load(*configPath)
	if err != nil {
		exit(err)
	}
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()
	if err := instagramconnector.New(cfg).Run(ctx); err != nil {
		exit(err)
	}
}

func exit(err error) {
	fmt.Fprintf(os.Stderr, "wuphf-instagram: %v\n", err)
	os.Exit(1)
}
