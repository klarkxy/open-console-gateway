package main

import (
	"context"

	sdkauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/auth"
	"github.com/router-for-me/CLIProxyAPI/v8/sdk/config"
)

func deviceLoginOptions() *sdkauth.LoginOptions {
	return &sdkauth.LoginOptions{
		Metadata: map[string]string{"codex_login_mode": "device"},
	}
}

// runCodexDeviceLogin uses the existing Codex authenticator and file token store.
// The store truncates an existing auth file in place, so private permissions stay.
func runCodexDeviceLogin(ctx context.Context, cfg *config.Config) error {
	store := sdkauth.NewFileTokenStore()
	if cfg != nil {
		store.SetBaseDir(cfg.AuthDir)
	}
	manager := sdkauth.NewManager(store, sdkauth.NewCodexAuthenticator())
	_, _, err := manager.Login(ctx, "codex", cfg, deviceLoginOptions())
	return err
}
