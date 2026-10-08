// Exercises the native update API with isolated configurations supplied by the caller.
import assert from "node:assert/strict";
import { readFile, writeFile, readdir, rename, rm, stat, cp } from "node:fs/promises";
import { join, dirname, basename } from "node:path";

function extra(client, text, value) {
  const field = value === "keep-after-takeover" ? "user_after" : "user_note";
  if (client === "codex") return text.replace("[model_providers.ocg]", `[model_providers.ocg]\n${field} = "${value}"`);
  if (client === "kimi") return text.replace("[providers.ocg-messages]", `[providers.ocg-messages]\n${field} = "${value}"`);
  if (client === "minimax") return text.replace("  ocg-messages:", `  ocg-messages:\n    ${field}: ${value}`);
  const doc = JSON.parse(text);
  if (client === "zcode") doc.config.providerConfigRules.providerRules.find(row => row.providerId === "ocg-messages")[field] = value;
  else doc[0][field] = value;
  return JSON.stringify(doc, null, 2);
}

export async function exerciseByokUpdates({ endpoint, client, targetPath, data, secrets, assertNoSecrets, accountId, publicModels }) {
  const call = async (suffix, method = "GET", body) => {
    const response = await fetch(`${endpoint}/applications/byok/${client}${suffix}`, {
      method, headers: { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const value = await response.json();
    assertNoSecrets(value, secrets);
    return { status: response.status, value };
  };
  const inspect = async () => {
    const result = await call(`?targetPath=${encodeURIComponent(targetPath)}`);
    assert.equal(result.status, 200, client);
    return result.value;
  };
  const prepare = async () => {
    const before = await readFile(targetPath);
    const result = await call("/preview", "POST", { targetPath });
    assert.equal(result.status, 200, `${client} preview: ${JSON.stringify(result.value)}`);
    assert.ok(result.value.preview?.planFingerprint);
    assert.deepEqual(await readFile(targetPath), before, "preview changed native bytes");
    return result.value;
  };
  const commit = async (view, acknowledgments = {}) => call("", "POST", {
    targetPath, expectedRevision: view.revision.revision,
    processGeneration: view.revision.processGeneration,
    expectedFingerprint: view.fingerprint, previewFingerprint: view.preview.planFingerprint,
    clientClosed: true, ...acknowledgments,
  });
  const remove = async view => call("", "DELETE", {
    targetPath, expectedRevision: view.revision.revision,
    processGeneration: view.revision.processGeneration,
    expectedFingerprint: view.fingerprint, clientClosed: true,
  });

  const catalog = async ids => {
    const contract = await fetch(`${endpoint}/contract`).then(response => response.json());
    const response = await fetch(`${endpoint}/accounts/${accountId}/custom-config`, {
      method: "PUT", headers: { "content-type": "application/json" },
      body: JSON.stringify({ expectedRevision: contract.revision, processGeneration: contract.processGeneration,
        endpointUrl: "https://example.test/v1/messages", upstreamProtocol: "messages",
        modelCapabilities: ids.map(id => ({ publicModel: id, upstreamModel: id, protocol: "messages" })),
      }),
    });
    const body = await response.json(); assertNoSecrets(body, secrets);
    assert.equal(response.status, 200, `${client} catalog update: ${JSON.stringify(body)}`);
  };
  const added = "vendor/added-on-update";
  const removed = publicModels[0];
  for (const [ids, addedIds, removedIds] of [
    [[...publicModels, added], [added], []],
    [[...publicModels.filter(id => id !== removed), added], [], [removed]],
    [publicModels, [removed], [added]],
  ]) {
    await catalog(ids);
    const plan = await prepare();
    assert.deepEqual(plan.preview.addedModelIds, addedIds);
    assert.deepEqual(plan.preview.removedModelIds, removedIds);
    const changed = await commit(plan);
    assert.equal(changed.status, 200, `${client} catalog refresh: ${JSON.stringify(changed.value)}`);
    assert.deepEqual([...changed.value.configuredModelIds].sort(), [...ids].sort());
    if (client !== "copilot") assert.ok(ids.includes(changed.value.defaultModelId), "default points to a removed model");
  }
  const initial = await prepare();
  assert.equal(initial.preview.requiresTakeover, false);
  assert.equal(initial.preview.requiresOverwrite, false);
  if (process.platform === "win32") {
    const alias = join(dirname(targetPath), basename(targetPath).toUpperCase());
    const actual = await stat(targetPath, { bigint: true });
    const aliased = await stat(alias, { bigint: true });
    if (actual.ino === aliased.ino && actual.dev === aliased.dev) {
      const result = await call("/preview", "POST", { targetPath: alias });
      assert.equal(result.status, 200, `${client} equivalent path preview`);
      assert.equal(result.value.preview.requiresTakeover, false, "same Windows file spelling lost ownership");
      assert.equal(result.value.fingerprint, initial.fingerprint, "same file must share an inspected fingerprint");
    }
  }
  const staleBytes = await readFile(targetPath);
  await writeFile(targetPath, extra(client, staleBytes.toString(), "keep-update"));
  const stale = await commit(initial);
  assert.equal(stale.status, 409, `${client} stale preview must fail`);
  const current = await prepare();
  assert.equal(current.preview.requiresOverwrite, false, "unknown provider field is not an owned conflict");
  const updated = await commit(current);
  assert.equal(updated.status, 200, `${client} extra field update: ${JSON.stringify(updated.value)}`);
  assert.ok((await readFile(targetPath, "utf8")).includes("keep-update"));

  if (client === "codex") {
    const view = await inspect();
    const catalogPath = view.targetPaths.find(path => path !== view.configPath);
    const catalog = JSON.parse(await readFile(catalogPath, "utf8"));
    await writeFile(catalogPath, JSON.stringify(catalog));
    const formatted = await prepare();
    assert.equal(formatted.preview.requiresOverwrite, false, "catalog JSON formatting is not a semantic edit");
    assert.equal((await commit(formatted)).status, 200);
  }

  if (client === "copilot") {
    const config = JSON.parse(await readFile(targetPath, "utf8"));
    config[0].models[0].maxOutputTokens = 4000;
    await writeFile(targetPath, JSON.stringify(config, null, 2));
    for (let count = 0; count < 2; count += 1) {
      const fresh = await prepare();
      assert.equal(fresh.preview.requiresOverwrite, false);
      const saved = await commit(fresh);
      assert.equal(saved.status, 200, "compatible client budget must refresh");
      assert.equal(JSON.parse(await readFile(targetPath, "utf8"))[0].models[0].maxOutputTokens, 4000,
        "second unchanged refresh reset a client-owned budget");
    }
  }

  // Losing ONLY the Host receipt must not leave a valid native OCG entry unusable.
  // Stale origin backups remain, so adoption must save a NEW current baseline.
  // Keep the old private state outside the managed client directory for cleanup.
  const clientStore = join(data, "applications", "byok", client);
  const folders = (await readdir(clientStore)).filter(name => !name.startsWith("."));
  assert.equal(folders.length, 1, `${client} unexpected receipt stores`);
  const oldStore = join(clientStore, folders[0]);
  const savedStore = join(data, `.saved-${client}-receipt`);
  await cp(oldStore, savedStore, { recursive: true });
  await rm(join(oldStore, "receipt.json"));
  const baseline = await readFile(targetPath, "utf8");
  try {
    const takeover = await prepare();
    assert.equal(takeover.preview.requiresTakeover, true, `${client} takeover missing`);
    assert.equal((await commit(takeover)).status, 409, "takeover requires explicit acknowledgment");
    const adopted = await commit(takeover, { acknowledgeTakeover: true });
    assert.equal(adopted.status, 200, `${client} takeover: ${JSON.stringify(adopted.value)}`);
    assert.equal(adopted.value.adopted, true);
    // A later user-owned field INSIDE the adopted block must survive Undo takeover.
    await writeFile(targetPath, extra(client, await readFile(targetPath, "utf8"), "keep-after-takeover"));
    const undo = await remove(await inspect());
    assert.equal(undo.status, 200, `${client} undo takeover: ${JSON.stringify(undo.value)}`);
    const undone = await readFile(targetPath, "utf8");
    assert.ok(undone.includes("keep-after-takeover"), `${client} undo lost later user fields`);
    assert.ok(undone.includes("keep-update"), `${client} undo lost baseline user fields`);
  } finally {
    // The remainder of the original smoke checks the normal managed removal.
    await writeFile(targetPath, baseline);
    await rm(oldStore, { recursive: true, force: true });
    await rename(savedStore, oldStore);
  }
  return { view: await inspect(), checks: ["catalog-add-remove-readd", "read-only-preview", "stale-file-refusal", "preserved-provider-extra", "takeover-acknowledgment", "undo-preserves-later-extra", ...(client === "codex" ? ["semantic-catalog-format"] : [])] };
}
