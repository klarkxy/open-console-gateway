package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"strings"
)

func main() {
	flags := flag.NewFlagSet("ocg-cpa-host", flag.ContinueOnError)
	flags.SetOutput(os.Stderr)
	configPath := flags.String("config", "", "path to the private CPA config.yaml written by OCG")
	deviceLogin := flags.Bool("codex-device-login", false, "run the existing Codex device-login flow and exit")
	if err := flags.Parse(os.Args[1:]); err != nil {
		os.Exit(2)
	}
	if strings.TrimSpace(*configPath) == "" {
		fmt.Fprintln(os.Stderr, "ocg-cpa-host requires --config")
		os.Exit(2)
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	defer stop()
	if *deviceLogin {
		_, _, cfg, _, err := loadPrivateConfig(*configPath)
		if err != nil {
			fmt.Fprintln(os.Stderr, err.Error())
			os.Exit(1)
		}
		if err := runCodexDeviceLogin(ctx, cfg); err != nil {
			fmt.Fprintln(os.Stderr, err.Error())
			os.Exit(1)
		}
		return
	}
	host, err := Start(ctx, *configPath)
	if err != nil {
		fmt.Fprintln(os.Stderr, err.Error())
		os.Exit(1)
	}
	defer host.Close()
	fmt.Fprintf(os.Stderr, "ocg-cpa-host ready %s\n", host.URL())
	<-ctx.Done()
}
