import assert from "node:assert/strict";
import test from "node:test";
import {
  accountInferenceEndpointUrl,
  officialBalanceSupported,
} from "./upstream-balance.ts";
import type { Account } from "../api/dashboard.ts";
import type { Connection } from "../api/connections.ts";
import type { Identity } from "../api/identities.ts";

test("official balance hosts are exact names only", () => {
  assert.equal(officialBalanceSupported("https://api.deepseek.com/chat/completions"), true);
  assert.equal(officialBalanceSupported("https://api.moonshot.cn/v1/chat/completions"), true);
  assert.equal(officialBalanceSupported("https://api.moonshot.ai/v1/chat/completions"), true);
  assert.equal(officialBalanceSupported("https://api.deepseek.com.evil.example/chat/completions"), false);
  assert.equal(officialBalanceSupported("https://api.openai.com/v1/chat/completions"), false);
  assert.equal(officialBalanceSupported("not a url"), false);
  assert.equal(officialBalanceSupported(""), false);
  assert.equal(officialBalanceSupported(null), false);
});

test("account inference URL prefers the Custom Endpoint then the connection", () => {
  const custom: Pick<Account, "custom_config"> = {
    custom_config: {
      account_id: "a",
      endpoint_url: "https://api.deepseek.com/chat/completions",
      upstream_protocol: "chat_completions",
      created_at: "",
      updated_at: "",
    },
  };
  assert.equal(
    accountInferenceEndpointUrl(custom, null, null),
    "https://api.deepseek.com/chat/completions",
  );

  const identity = {
    legacy: { kind: "account", id: "acc-1" },
    credentials: [{
      legacy: { kind: "account", id: "acc-1" },
      bindings: [{ connection_id: "conn-1", allowed_endpoint_ids: [], allowed_origins: [], model_scope: { type: "all" }, enabled: true, routing_rank: 0, id: "bind-1" }],
    }],
  } as unknown as Identity;
  const connections = [{
    id: "conn-1",
    endpoints: [{ url: "https://api.moonshot.cn/v1/chat/completions" }],
  }] as unknown as Connection[];
  assert.equal(
    accountInferenceEndpointUrl({ custom_config: null }, identity, connections),
    "https://api.moonshot.cn/v1/chat/completions",
  );
});
