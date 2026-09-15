import { randomBytes } from "node:crypto";
import { constants as fsConstants } from "node:fs";
import { chmod, copyFile, readFile, readdir, rename, unlink } from "node:fs/promises";
import { findPackageJSON } from "node:module";
import { basename, dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

if (process.argv[1] === undefined) {
  throw new Error("Open Console Gateway could not identify the active DSH runtime.");
}

const runtimeBase = pathToFileURL(process.argv[1]).href;

async function importFromDsh(packageName, entry) {
  const manifest = findPackageJSON(packageName, runtimeBase);
  if (manifest === undefined) {
    throw new Error(`Open Console Gateway requires ${packageName} from the active DSH runtime.`);
  }
  return import(pathToFileURL(join(dirname(manifest), entry)).href);
}

const [piAi, openAiApi, dshLlm, dshPiAi] = await Promise.all([
  importFromDsh("@earendil-works/pi-ai", "dist/index.js"),
  importFromDsh("@earendil-works/pi-ai", "dist/api/openai-completions.lazy.js"),
  importFromDsh("@deepseek-ai/dsh-llm", "lib/index.js"),
  importFromDsh("@deepseek-ai/dsh-llm-pi-ai", "lib/index.js"),
]);

const { InMemoryCredentialStore, createProvider } = piAi;
const { openAICompletionsApi } = openAiApi;
const { LlmError, assertUsableApiKey, resolveRetryPolicy } = dshLlm;
const { PiAiAdapter } = dshPiAi;

export const name = "open-console-gateway-dsh";
export const inject = ["llm", "credentials"];

const providerId = "open-console-gateway";
const displayName = "Open Console Gateway";
const baseUrl = "__OCG_GATEWAY_V1_URL__";
const credentialRef = "OCG_GATEWAY_KEY";
const credentialBootstrapPath = __OCG_CREDENTIAL_BOOTSTRAP_PATH_JSON__;
const catalogTtlMs = 5_000;
const catalogTimeoutMs = 10_000;

function modelDefinition(id) {
  return {
    id,
    provider: providerId,
    api: "openai-completions",
    baseUrl,
    name: id,
    reasoning: false,
    input: ["text"],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    contextWindow: 128_000,
    maxTokens: 16_384,
  };
}

function parseModelCatalog(value) {
  if (value === null || typeof value !== "object" || !Array.isArray(value.data)) {
    throw new LlmError("Open Console Gateway returned an invalid /v1/models payload", "INVALID_CONFIG");
  }
  const seen = new Set();
  const models = [];
  for (const row of value.data) {
    const id = typeof row?.id === "string" ? row.id.trim() : "";
    if (id.length === 0 || seen.has(id)) continue;
    seen.add(id);
    models.push(modelDefinition(id));
  }
  return models;
}

const HANDOFF_CLAIM_MARKER = ".claimed-";

// Handoff state machine (one DSH runtime apply() at a time; the Host may
// replace the live path while credentials.set is in flight):
//   live `credential-handoff`     — durable pending Key from the Host
//   `credential-handoff.claimed-*` — ephemeral ownership of a previously live
//                                    Key, owned only by the current apply()
// Live always wins over leftovers. If live is missing, the newest leftover
// claim is the pending Key (crash remnant). After consume succeeds, or after
// a failed set is reconciled with live, no claim files remain. Claim names
// never include the secret.

function handoffClaimPrefix(livePath) {
  return `${basename(livePath)}${HANDOFF_CLAIM_MARKER}`;
}

function claimToken() {
  return `${String(Date.now()).padStart(16, "0")}-${randomBytes(8).toString("hex")}`;
}

async function unlinkIfPresent(path) {
  try {
    await unlink(path);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
}

async function listClaimedHandoffs(livePath) {
  const directory = dirname(livePath);
  const prefix = handoffClaimPrefix(livePath);
  let entries;
  try {
    entries = await readdir(directory, { withFileTypes: true });
  } catch (error) {
    if (error?.code === "ENOENT") return [];
    throw error;
  }
  return entries
    .filter((entry) => entry.isFile() && entry.name.startsWith(prefix))
    .map((entry) => join(directory, entry.name));
}

function newestClaim(paths) {
  if (paths.length === 0) return undefined;
  return paths.reduce((latest, path) => (
    basename(path).localeCompare(basename(latest)) > 0 ? path : latest
  ));
}

async function sweepClaimedHandoffs(livePath) {
  const claims = await listClaimedHandoffs(livePath);
  claims.sort((left, right) => basename(left).localeCompare(basename(right)));
  for (const path of claims) {
    await unlinkIfPresent(path);
  }
}

async function claimCredentialHandoff(livePath) {
  const claimPath = `${livePath}${HANDOFF_CLAIM_MARKER}${claimToken()}`;
  try {
    await rename(livePath, claimPath);
    return claimPath;
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  return newestClaim(await listClaimedHandoffs(livePath));
}

async function restoreClaimedHandoff(claimPath, livePath) {
  try {
    await copyFile(claimPath, livePath, fsConstants.COPYFILE_EXCL);
  } catch (error) {
    if (error?.code === "EEXIST") {
      await unlinkIfPresent(claimPath);
      return;
    }
    throw error;
  }
  try {
    await chmod(livePath, 0o600);
  } catch {
    // Best-effort; Windows may ignore POSIX modes. The live file remains for retry.
  }
  await unlinkIfPresent(claimPath);
}

async function consumeCredentialHandoff(credentials) {
  const livePath = credentialBootstrapPath;
  const claimPath = await claimCredentialHandoff(livePath);
  if (claimPath === undefined) return;
  let bootstrap;
  try {
    bootstrap = await readFile(claimPath, "utf8");
  } catch (error) {
    if (error?.code === "ENOENT") {
      await sweepClaimedHandoffs(livePath);
      return;
    }
    throw error;
  }
  if (bootstrap.length === 0) {
    await restoreClaimedHandoff(claimPath, livePath);
    await sweepClaimedHandoffs(livePath);
    throw new Error("Open Console Gateway credential handoff is empty.");
  }
  try {
    await credentials.set(credentialRef, bootstrap);
  } catch (error) {
    try {
      await restoreClaimedHandoff(claimPath, livePath);
      await sweepClaimedHandoffs(livePath);
    } catch {
      // Keep the claim for retry when live could not be restored.
    }
    throw error;
  }
  await sweepClaimedHandoffs(livePath);
}

export async function apply(ctx) {
  try {
    await consumeCredentialHandoff(ctx.get("credentials"));
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }

  let profiles = new Map();
  let refreshedAt = 0;
  let refreshPromise;

  const resolveApiKey = async () => {
    const stored = await ctx.get("credentials")?.resolve(credentialRef);
    const value = stored?.value ?? process.env[credentialRef];
    if (value !== undefined && value.length > 0) {
      return assertUsableApiKey(value, name, credentialRef);
    }
    throw new LlmError(
      `${name}: no credential stored for ${credentialRef}`,
      "MISSING_CREDENTIAL",
    );
  };

  const refreshCatalog = async (force = false) => {
    if (!force && profiles.size > 0 && Date.now() - refreshedAt < catalogTtlMs) return;
    if (refreshPromise !== undefined) return refreshPromise;
    refreshPromise = (async () => {
      const apiKey = await resolveApiKey();
      const response = await fetch(`${baseUrl}/models`, {
        headers: { Authorization: `Bearer ${apiKey}` },
        signal: AbortSignal.timeout(catalogTimeoutMs),
      });
      if (!response.ok) {
        throw new LlmError(
          `Open Console Gateway model discovery failed with HTTP ${response.status}`,
          "INVALID_CONFIG",
        );
      }
      const models = parseModelCatalog(await response.json());
      const piProvider = createProvider({
        id: providerId,
        name: displayName,
        baseUrl,
        auth: {
          apiKey: {
            name: `${displayName} Key`,
            async resolve({ credential }) {
              return {
                auth: credential?.key ? { apiKey: credential.key } : {},
                source: displayName,
              };
            },
          },
        },
        models,
        api: openAICompletionsApi(),
      });
      profiles = new Map([
        [
          providerId,
          {
            provider: providerId,
            displayName,
            apiKeyEnv: credentialRef,
            api: "openai-completions",
            baseURL: baseUrl,
            streamIdleTimeoutMs: 300_000,
            maxRequestImageBytes: 20 * 1024 * 1024,
            requestImagePixelBudget: 2048 * 2048,
            requestImageMaxBytes: 1024 * 1024,
            retryPolicy: resolveRetryPolicy(undefined, name),
            configuredMaxTokens: new Map(),
            modelErrors: new Map(),
            piProvider,
          },
        ],
      ]);
      refreshedAt = Date.now();
    })().finally(() => {
      refreshPromise = undefined;
    });
    return refreshPromise;
  };

  class OcgAdapter extends PiAiAdapter {
    async listModels(provider) {
      await refreshCatalog(true);
      return super.listModels(provider);
    }

    async resolveModel(provider, model, signal) {
      await refreshCatalog();
      return super.resolveModel(provider, model, signal);
    }

    async prepareCall(provider, model, signal) {
      await refreshCatalog();
      return super.prepareCall(provider, model, signal);
    }
  }

  const adapter = new OcgAdapter({
    profiles: () => profiles,
    resolveApiKey,
    auth: {
      credentials: new InMemoryCredentialStore(),
      authContext: {
        async env(variable) {
          return process.env[variable];
        },
        async fileExists() {
          return false;
        },
      },
    },
    resolveAttachments: () => ctx.get("attachments"),
  });

  ctx.llm.registerAdapter([providerId], adapter);
}
