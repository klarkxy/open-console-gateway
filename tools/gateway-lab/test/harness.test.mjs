import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { loadProfile } from "../lib/profile.mjs";
import { portOpen, processAlive } from "../lib/process.mjs";
import { withRuntime } from "../lib/harness.mjs";
import { defaultCliPath } from "../lib/process.mjs";
import { existsSync } from "node:fs";

async function assertClean(error) {
  const proof = error.cleanup;
  assert.ok(proof, "cleanup proof missing on setup failure");
  assert.equal(proof.listenersClosed, true, JSON.stringify(proof.listeners));
  assert.equal(proof.controlClosed, true, JSON.stringify(proof.control));
  assert.equal(proof.gatewayPidExited, true, JSON.stringify(proof.gateway));
  assert.equal(proof.gatewayPortClosed, true, JSON.stringify(proof.gateway));
  for (const listener of proof.listeners || []) {
    assert.equal(await portOpen(listener.port), false, `listener ${listener.id} still open`);
  }
  if (proof.control?.port) assert.equal(await portOpen(proof.control.port), false);
  if (proof.gateway?.pid) assert.equal(processAlive(proof.gateway.pid), false);
  if (proof.gateway?.port) assert.equal(await portOpen(proof.gateway.port), false);
}

test("setup failure after lab start closes owned listeners and control", async () => {
  const cliPath = defaultCliPath();
  if (!existsSync(cliPath)) return;
  const artifactDir = await mkdtemp(path.join(os.tmpdir(), "gw-lab-"));
  const profile = await loadProfile("default");
  await assert.rejects(async () => {
    try {
      await withRuntime({ cliPath, profile, artifactDir, register: false, inject: { failAfter: "labStart" } });
    } catch (error) {
      await assertClean(error);
      throw error;
    }
  }, /injected failure after lab start/);
});

test("setup failure after spawn stops only the owned child and lab ports", async () => {
  const cliPath = defaultCliPath();
  if (!existsSync(cliPath)) return;
  const artifactDir = await mkdtemp(path.join(os.tmpdir(), "gw-lab-"));
  const profile = await loadProfile("default");
  await assert.rejects(async () => {
    try {
      await withRuntime({ cliPath, profile, artifactDir, register: false, inject: { failAfter: "spawn" } });
    } catch (error) {
      await assertClean(error);
      assert.ok(error.cleanup.gateway.pid, "owned child pid missing");
      throw error;
    }
  }, /injected failure after spawn/);
});
