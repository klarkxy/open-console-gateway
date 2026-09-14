import assert from "node:assert/strict";
import test from "node:test";
import {
  maskConnectionKey,
  normalizeClientRootUrl,
  resolveConnectionUrls,
} from "./dashboard-connection.ts";

test("connection helpers mask display values", () => {
  assert.equal(maskConnectionKey(""), "未设置");
  assert.equal(maskConnectionKey("tinykey"), "ti…ey");
  assert.equal(maskConnectionKey("ocg-1234567890"), "ocg-…7890");
});

test("client root normalization accepts roots and strips only a terminal v1", () => {
  assert.equal(normalizeClientRootUrl(""), "");
  assert.equal(normalizeClientRootUrl("   "), "");
  assert.equal(normalizeClientRootUrl(" https://ocg.example.com/// "), "https://ocg.example.com");
  assert.equal(normalizeClientRootUrl("https://ocg.example.com/proxy/"), "https://ocg.example.com/proxy");
  assert.equal(normalizeClientRootUrl("https://ocg.example.com/proxy/v1/"), "https://ocg.example.com/proxy");
  assert.equal(normalizeClientRootUrl("http://192.168.1.8:9042/ocg"), "http://192.168.1.8:9042/ocg");
});

test("client root normalization rejects endpoints and unsafe URL structure", () => {
  for (const value of [
    "ocg.example.com",
    "http:ocg.example.com",
    "http:/ocg.example.com",
    "/dashboard",
    "ftp://ocg.example.com",
    "https://user:password@ocg.example.com",
    "https://ocg.example.com?node=one",
    "https://ocg.example.com#settings",
    "https://ocg.example.com/v1/chat/completions",
    "https://ocg.example.com/proxy/v1/responses",
  ]) {
    assert.throws(() => normalizeClientRootUrl(value), Error, value);
  }
});

test("connection URL derivation handles configured, development, and production roots", () => {
  assert.deepEqual(
    resolveConnectionUrls("", "http://127.0.0.1:30001", 9042, true),
    {
      rootUrl: "http://127.0.0.1:9042",
      apiBaseUrl: "http://127.0.0.1:9042/v1",
      chatCompletionsUrl: "http://127.0.0.1:9042/v1/chat/completions",
      responsesUrl: "http://127.0.0.1:9042/v1/responses",
      messagesUrl: "http://127.0.0.1:9042/v1/messages",
      insecureHttp: false,
    },
  );
  assert.equal(
    resolveConnectionUrls("", "https://ocg.example.com", 9042, false).apiBaseUrl,
    "https://ocg.example.com/v1",
  );
  const configured = resolveConnectionUrls(
    "https://edge.example.com/ocg/v1/",
    "https://ignored.example.com",
    9042,
    false,
  );
  assert.equal(configured.rootUrl, "https://edge.example.com/ocg");
  assert.equal(configured.apiBaseUrl, "https://edge.example.com/ocg/v1");
  assert.doesNotMatch(configured.apiBaseUrl, /\/v1\/v1/);
  assert.equal(resolveConnectionUrls("http://localhost:9042", "https://ignored", 9042, false).insecureHttp, false);
  assert.equal(resolveConnectionUrls("http://127.0.0.8:9042", "https://ignored", 9042, false).insecureHttp, false);
  assert.equal(resolveConnectionUrls("http://192.168.1.8:9042", "https://ignored", 9042, false).insecureHttp, true);
  assert.equal(resolveConnectionUrls("https://192.168.1.8:9042", "https://ignored", 9042, false).insecureHttp, false);
});
