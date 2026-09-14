import assert from "node:assert/strict";
import test from "node:test";
import type { AppConfig } from "../api/dashboard.ts";
import { mergeUnsavedSettings } from "./settings-merge.ts";

function config(overrides: Partial<AppConfig> = {}): AppConfig {
  return {
    revision: 1,
    gateway_port: 9042,
    gateway_port_from_env: false,
    proxy_mode: "auto",
    proxy_url: "",
    proxy_list_direction: "whitelist",
    proxy_list_models: [],
    proxy_supported_models: [],
    opencode_invite_url: "https://opencode.ai/go?ref=68XPB6NP8V",
    client_root_url: "",
    client_root_url_from_env: false,
    auto_start: false,
    auto_start_supported: true,
    show_dock_icon: true,
    dock_visibility_supported: false,
    connect_timeout_secs: 30,
    non_stream_timeout_secs: 900,
    stream_idle_timeout_secs: 300,
    routing_mode: "strict-priority",
    conversation_sticky: true,
    ...overrides,
  };
}

test("settings conflict merge preserves local edits and accepts unrelated remote edits", () => {
  const saved = config();
  const current = config({
    proxy_mode: "manual",
    proxy_url: "http://127.0.0.1:7890",
    connect_timeout_secs: 45,
  });
  const latest = config({
    revision: 2,
    gateway_port: 9142,
    non_stream_timeout_secs: 1_200,
  });

  const merged = mergeUnsavedSettings(latest, current, saved);

  assert.equal(merged.revision, 2);
  assert.equal(merged.proxy_mode, "manual");
  assert.equal(merged.proxy_url, "http://127.0.0.1:7890");
  assert.equal(merged.connect_timeout_secs, 45);
  assert.equal(merged.gateway_port, 9142);
  assert.equal(merged.non_stream_timeout_secs, 1_200);
});

test("settings conflict merge adopts the remote OpenCode Go invite URL", () => {
  const saved = config();
  const current = config({
    opencode_invite_url: "https://opencode.ai/invite/local",
    proxy_mode: "manual",
  });
  const latest = config({
    revision: 2,
    opencode_invite_url: "https://opencode.ai/invite/remote",
  });

  const merged = mergeUnsavedSettings(latest, current, saved);

  assert.equal(merged.opencode_invite_url, "https://opencode.ai/invite/remote");
  assert.equal(merged.proxy_mode, "manual");
});

test("settings conflict merge keeps local edits and adopts server capability flags", () => {
  const saved = config();
  const current = config({ auto_start: true });
  const latest = config({
    revision: 3,
    auto_start_supported: false,
    client_root_url_from_env: true,
    gateway_port_from_env: true,
  });

  const merged = mergeUnsavedSettings(latest, current, saved);

  assert.equal(merged.auto_start, true);
  assert.equal(merged.auto_start_supported, false);
  assert.equal(merged.client_root_url_from_env, true);
  assert.equal(merged.gateway_port_from_env, true);
});

test("an untouched clone of the saved model list adopts the latest server array", () => {
  const saved = config({ proxy_list_models: ["m1", "m2"] });
  // The form clones the array on load: same content, different reference.
  const current = config({ proxy_list_models: [...saved.proxy_list_models] });
  const latest = config({ revision: 2, proxy_list_models: ["m1", "m2", "m3"] });

  const merged = mergeUnsavedSettings(latest, current, saved);

  assert.deepEqual(merged.proxy_list_models, ["m1", "m2", "m3"]);
});

test("a locally edited model list still wins over the server snapshot", () => {
  const saved = config({ proxy_list_models: ["m1", "m2"] });
  const current = config({ proxy_list_models: ["m1", "m3"] });
  const latest = config({ revision: 2, proxy_list_models: ["m1", "m2", "m4"] });

  const merged = mergeUnsavedSettings(latest, current, saved);

  assert.deepEqual(merged.proxy_list_models, ["m1", "m3"]);
});
