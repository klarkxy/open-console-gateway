import assert from "node:assert/strict";
import test from "node:test";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import * as v3 from "./dashboard-v3-contract.mjs";
import * as v4 from "./dashboard-v4-contract.mjs";
import {
  assertTypesAreContractOnly,
  createDashboardContract,
  parseArgs,
  resolveContractVersion,
  run,
} from "./lib/dashboard-contract.mjs";

const cases = [
  { version: "v3", contract: v3, rootTypeName: "DashboardApiV3" },
  { version: "v4", contract: v4, rootTypeName: "DashboardApiV4" },
];

function fixtureSchema(rootTypeName) {
  return {
    $schema: "https://json-schema.org/draft/2020-12/schema",
    title: rootTypeName,
    anyOf: [{ $ref: "#/$defs/ControlRevision" }],
    $defs: {
      ControlRevision: {
        type: "object",
        additionalProperties: false,
        required: ["revision", "processGeneration"],
        properties: {
          revision: { type: "integer", minimum: 0 },
          processGeneration: { type: "integer", minimum: 0 },
        },
      },
    },
  };
}

test("CLI accepts one mode and supported contract versions", () => {
  assert.equal(parseArgs(["--write"]), "write");
  assert.equal(parseArgs(["--check"]), "check");
  assert.throws(() => parseArgs([]));
  assert.throws(() => parseArgs(["--write", "--check"]));
  assert.throws(() => parseArgs(["--client"]));
  for (const { version } of cases) {
    assert.equal(resolveContractVersion(version).version, version);
  }
  assert.throws(() => resolveContractVersion("v2"));
  assert.throws(() => createDashboardContract("v5"));
});

test("generated TypeScript rejects HTTP clients and executable functions", () => {
  assert.doesNotThrow(() => assertTypesAreContractOnly("export interface Revision { revision: number }"));
  assert.throws(() => assertTypesAreContractOnly("export async function getContract() { return fetch('/contract'); }"));
  assert.throws(() => assertTypesAreContractOnly("export function getContract() { return 1; }"));
});

for (const { version, contract, rootTypeName } of cases) {
  test(`${version} writes only its artifacts and checks drift without writing`, async () => {
    const root = fileURLToPath(new URL(`../tmp/contract-test/${version}/`, import.meta.url));
    const schemaPath = join(root, `schema/dashboard-api-${version}.schema.json`);
    const typesPath = join(root, `src/api/generated/dashboard-${version}.ts`);
    const schema = `${JSON.stringify(fixtureSchema(rootTypeName), null, 2)}\n`;
    const types = "export interface ControlRevision { revision: number; }\n";
    const writes = [];
    const deps = {
      root,
      exportSchema: () => schema,
      compileSchema: async () => types,
      readText: (path) => path === schemaPath ? schema : types,
      writeText: (path, contents) => writes.push({ path, contents }),
    };

    await run({ version, argv: ["--write"], ...deps });
    assert.deepEqual(writes, [
      { path: schemaPath, contents: schema },
      { path: typesPath, contents: types },
    ]);
    writes.length = 0;
    await contract.runContract("check", deps);
    for (const driftedPath of [schemaPath, typesPath]) {
      await assert.rejects(() => contract.runContract("check", {
        ...deps,
        readText: (path) => path === driftedPath ? "drifted\n" : deps.readText(path),
      }), /contract drifted/);
    }
    assert.deepEqual(writes, []);
  });

  test(`${version} renders the schema as types with its required fields`, async () => {
    const ts = await contract.renderTypeScript(fixtureSchema(rootTypeName));
    assert.match(ts, /export (interface|type) ControlRevision/);
    assert.match(ts, /processGeneration/);
    assertTypesAreContractOnly(ts);
  });

  test(`${version} invokes its Rust schema exporter`, () => {
    const calls = [];
    const schemaText = contract.exportSchemaFromCargo({
      spawn: (cmd, args) => {
        calls.push({ cmd, args });
        return { status: 0, stdout: '{"title":"ok"}\n', stderr: "" };
      },
    });
    assert.equal(schemaText, '{"title":"ok"}\n');
    assert.deepEqual(calls, [{
      cmd: "cargo",
      args: ["run", "-p", "ocg-core", "--example", `export_dashboard_${version}_schema`, "--locked", "--quiet"],
    }]);
  });
}
