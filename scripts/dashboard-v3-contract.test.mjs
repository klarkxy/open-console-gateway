import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  ARTIFACT_PATHS,
  CARGO_ARGS,
  CARGO_EXAMPLE,
  SCHEMA_RELATIVE_PATH,
  TYPES_RELATIVE_PATH,
  assertTypesAreContractOnly,
  parseArgs,
  renderTypeScript,
  runContract,
} from "./dashboard-v3-contract.mjs";
import {
  VERSIONS,
  createDashboardContract,
  formatDriftError,
  resolveContractVersion,
} from "./lib/dashboard-contract.mjs";

const fixtureSchema = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  title: "DashboardApiV3",
  anyOf: [{ $ref: "#/$defs/ControlRevision" }],
  $defs: {
    ControlRevision: {
      type: "object",
      additionalProperties: false,
      required: ["revision", "processGeneration", "pricingRevision"],
      properties: {
        revision: { type: "integer", minimum: 0 },
        processGeneration: { type: "integer", minimum: 0 },
        pricingRevision: { type: "string" },
      },
    },
  },
};

test("parseArgs accepts exactly one of write or check", () => {
  assert.equal(parseArgs(["--write"]), "write");
  assert.equal(parseArgs(["--check"]), "check");
  assert.throws(() => parseArgs([]), /exactly one/);
  assert.throws(() => parseArgs(["--write", "--check"]), /exactly one/);
  assert.throws(() => parseArgs(["--client"]), /Unknown argument/);
});

test("generated TypeScript must stay types-only", () => {
  assert.doesNotThrow(() => assertTypesAreContractOnly("export interface ControlRevision { revision: number }\n"));
  assert.throws(
    () => assertTypesAreContractOnly("export async function getContract() { return fetch('/dashboard/api/v3/contract'); }"),
    /types-only/,
  );
  assert.throws(
    () => assertTypesAreContractOnly("export function getContract() { return 1; }"),
    /types-only/,
  );
});

test("renderTypeScript emits interfaces without clients", async () => {
  const ts = await renderTypeScript(fixtureSchema);
  assert.match(ts, /export (interface|type) ControlRevision/);
  assert.match(ts, /processGeneration/);
});

test("check mode detects drift without writing", async () => {
  const writes = [];
  const generatedSchema = `${JSON.stringify(fixtureSchema, null, 2)}\n`;
  await runContract("check", {
    root: fileURLToPath(new URL("../", import.meta.url)),
    exportSchema: () => generatedSchema,
    compileSchema: async () => "export interface ControlRevision { revision: number; }\n",
    readText: (path) => {
      if (path.endsWith("dashboard-api-v3.schema.json")) return generatedSchema;
      return "export interface ControlRevision { revision: number; }\n";
    },
    writeText: (path, contents) => writes.push({ path, contents }),
  });
  assert.equal(writes.length, 0);

  await assert.rejects(
    () => runContract("check", {
      root: fileURLToPath(new URL("../", import.meta.url)),
      exportSchema: () => generatedSchema,
      compileSchema: async () => "export interface Drifted { revision: number; }\n",
      readText: () => "export interface ControlRevision { revision: number; }\n",
      writeText: (path, contents) => writes.push({ path, contents }),
    }),
    /drifted/,
  );
  assert.equal(writes.length, 0);
});

test("write mode only writes the two contract artifacts", async () => {
  const writes = [];
  const generatedSchema = `${JSON.stringify(fixtureSchema, null, 2)}\n`;
  await runContract("write", {
    root: "/tmp/ocg-v3-contract-test",
    exportSchema: () => generatedSchema,
    compileSchema: async () => "export interface ControlRevision { revision: number; }\n",
    writeText: (path, contents) => writes.push({ path: path.replaceAll("\\", "/"), contents }),
  });
  assert.deepEqual(
    writes.map((entry) => entry.path),
    [
      "/tmp/ocg-v3-contract-test/schema/dashboard-api-v3.schema.json",
      "/tmp/ocg-v3-contract-test/src/api/generated/dashboard-v3.ts",
    ],
  );
});

test("resolved V3 config equals the previous constants", () => {
  const previous = Object.freeze({
    schemaRelativePath: "schema/dashboard-api-v3.schema.json",
    typesRelativePath: "src/api/generated/dashboard-v3.ts",
    cargoExample: "export_dashboard_v3_schema",
    rootTypeName: "DashboardApiV3",
  });
  const spec = resolveContractVersion("v3");
  const contract = createDashboardContract("v3");

  assert.equal(spec, VERSIONS.v3);
  assert.equal(spec.schemaRelativePath, previous.schemaRelativePath);
  assert.equal(spec.typesRelativePath, previous.typesRelativePath);
  assert.equal(spec.cargoExample, previous.cargoExample);
  assert.equal(spec.rootTypeName, previous.rootTypeName);
  assert.equal(SCHEMA_RELATIVE_PATH, previous.schemaRelativePath);
  assert.equal(TYPES_RELATIVE_PATH, previous.typesRelativePath);
  assert.equal(CARGO_EXAMPLE, previous.cargoExample);
  assert.deepEqual(ARTIFACT_PATHS, [previous.schemaRelativePath, previous.typesRelativePath]);
  assert.deepEqual(CARGO_ARGS, [
    "run",
    "-p",
    "ocg-core",
    "--example",
    previous.cargoExample,
    "--locked",
    "--quiet",
  ]);
  const drift = formatDriftError(spec, [previous.schemaRelativePath, previous.typesRelativePath]);
  assert.ok(drift.includes(previous.schemaRelativePath));
  assert.ok(drift.includes(previous.typesRelativePath));
  assert.ok(drift.includes("pnpm run contract:v3:generate"));
  assert.throws(
    () => contract.assertArtifactsMatch({
      generatedSchema: "a\n",
      generatedTypes: "b\n",
      existingSchema: "A\n",
      existingTypes: "B\n",
    }),
    /schema\/dashboard-api-v3\.schema\.json.*src\/api\/generated\/dashboard-v3\.ts/s,
  );
});
