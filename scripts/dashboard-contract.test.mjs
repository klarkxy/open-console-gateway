import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  ARTIFACT_PATHS as V4_ARTIFACT_PATHS,
  CARGO_ARGS as V4_CARGO_ARGS,
  CARGO_EXAMPLE as V4_CARGO_EXAMPLE,
  SCHEMA_RELATIVE_PATH as V4_SCHEMA_RELATIVE_PATH,
  TYPES_RELATIVE_PATH as V4_TYPES_RELATIVE_PATH,
} from "./dashboard-v4-contract.mjs";
import {
  VERSIONS,
  createDashboardContract,
  formatDriftError,
  parseArgs,
  resolveContractVersion,
  run,
} from "./lib/dashboard-contract.mjs";

const cases = [
  {
    version: "v3",
    schemaFile: "dashboard-api-v3.schema.json",
    rootTypeName: "DashboardApiV3",
    cargoExample: "export_dashboard_v3_schema",
    schemaRelativePath: "schema/dashboard-api-v3.schema.json",
    typesRelativePath: "src/api/generated/dashboard-v3.ts",
    generateScript: "contract:v3:generate",
    label: "V3",
  },
  {
    version: "v4",
    schemaFile: "dashboard-api-v4.schema.json",
    rootTypeName: "DashboardApiV4",
    cargoExample: "export_dashboard_v4_schema",
    schemaRelativePath: "schema/dashboard-api-v4.schema.json",
    typesRelativePath: "src/api/generated/dashboard-v4.ts",
    generateScript: "contract:v4:generate",
    label: "V4",
  },
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
        required: ["revision", "processGeneration", "pricingRevision"],
        properties: {
          revision: { type: "integer", minimum: 0 },
          processGeneration: { type: "integer", minimum: 0 },
          pricingRevision: { type: "string" },
        },
      },
    },
  };
}

test("resolveContractVersion accepts only v3 and v4", () => {
  assert.equal(resolveContractVersion("v3"), VERSIONS.v3);
  assert.equal(resolveContractVersion("v4"), VERSIONS.v4);
  assert.throws(() => resolveContractVersion("v2"), /Unknown dashboard contract version/);
  assert.throws(() => createDashboardContract("v5"), /Unknown dashboard contract version/);
});

test("V4 entry re-exports the shared v4 constants", () => {
  assert.equal(V4_SCHEMA_RELATIVE_PATH, VERSIONS.v4.schemaRelativePath);
  assert.equal(V4_TYPES_RELATIVE_PATH, VERSIONS.v4.typesRelativePath);
  assert.equal(V4_CARGO_EXAMPLE, VERSIONS.v4.cargoExample);
  assert.deepEqual(V4_ARTIFACT_PATHS, [
    VERSIONS.v4.schemaRelativePath,
    VERSIONS.v4.typesRelativePath,
  ]);
  assert.deepEqual(V4_CARGO_ARGS, [
    "run",
    "-p",
    "ocg-core",
    "--example",
    "export_dashboard_v4_schema",
    "--locked",
    "--quiet",
  ]);
});

for (const scenario of cases) {
  test(`${scenario.version} factory write/check paths stay version-specific`, async () => {
    const contract = createDashboardContract(scenario.version);
    const generatedSchema = `${JSON.stringify(fixtureSchema(scenario.rootTypeName), null, 2)}\n`;
    const generatedTypes = "export interface ControlRevision { revision: number; }\n";
    const writes = [];

    await contract.runContract("write", {
      root: `/tmp/ocg-${scenario.version}-contract-test`,
      exportSchema: () => generatedSchema,
      compileSchema: async () => generatedTypes,
      writeText: (path, contents) => writes.push({ path: path.replaceAll("\\", "/"), contents }),
    });
    assert.deepEqual(
      writes.map((entry) => entry.path),
      [
        `/tmp/ocg-${scenario.version}-contract-test/${scenario.schemaRelativePath}`,
        `/tmp/ocg-${scenario.version}-contract-test/${scenario.typesRelativePath}`,
      ],
    );

    const checkWrites = [];
    await contract.runContract("check", {
      root: fileURLToPath(new URL("../", import.meta.url)),
      exportSchema: () => generatedSchema,
      compileSchema: async () => generatedTypes,
      readText: (path) => {
        if (path.endsWith(scenario.schemaFile)) return generatedSchema;
        return generatedTypes;
      },
      writeText: (path, contents) => checkWrites.push({ path, contents }),
    });
    assert.equal(checkWrites.length, 0);

    await assert.rejects(
      () => contract.runContract("check", {
        root: fileURLToPath(new URL("../", import.meta.url)),
        exportSchema: () => generatedSchema,
        compileSchema: async () => "export interface Drifted { revision: number; }\n",
        readText: () => generatedTypes,
        writeText: (path, contents) => checkWrites.push({ path, contents }),
      }),
      new RegExp(`Dashboard ${scenario.label} contract drifted`),
    );
    assert.equal(checkWrites.length, 0);
  });

  test(`${scenario.version} drift text names the generate script`, () => {
    const spec = resolveContractVersion(scenario.version);
    const message = formatDriftError(spec, [scenario.schemaRelativePath, scenario.typesRelativePath]);
    assert.equal(
      message,
      `Dashboard ${scenario.label} contract drifted (${scenario.schemaRelativePath}, ${scenario.typesRelativePath}). Run \`pnpm run ${scenario.generateScript}\`.`,
    );
  });

  test(`${scenario.version} renderTypeScript stays types-only`, async () => {
    const contract = createDashboardContract(scenario.version);
    const ts = await contract.renderTypeScript(fixtureSchema(scenario.rootTypeName));
    assert.match(ts, /export (interface|type) ControlRevision/);
    assert.match(ts, new RegExp(`AUTO-GENERATED by scripts/dashboard-${scenario.version}-contract\\.mjs`));
    assert.doesNotMatch(ts, /\bfetch\s*\(/);
    assert.doesNotMatch(ts, /export async function/);
  });

  test(`${scenario.version} exportSchemaFromCargo uses the version example`, () => {
    const contract = createDashboardContract(scenario.version);
    const calls = [];
    const schemaText = contract.exportSchemaFromCargo({
      spawn: (cmd, args, options) => {
        calls.push({ cmd, args, cwd: options.cwd });
        return { status: 0, stdout: '{"title":"ok"}\n', stderr: "" };
      },
    });
    assert.equal(schemaText, '{"title":"ok"}\n');
    assert.equal(calls.length, 1);
    assert.equal(calls[0].cmd, "cargo");
    assert.deepEqual(calls[0].args, [
      "run",
      "-p",
      "ocg-core",
      "--example",
      scenario.cargoExample,
      "--locked",
      "--quiet",
    ]);
  });

  test(`run({ version: "${scenario.version}" }) honors --write without cargo`, async () => {
    const writes = [];
    const generatedSchema = `${JSON.stringify(fixtureSchema(scenario.rootTypeName), null, 2)}\n`;
    await run({
      version: scenario.version,
      argv: ["--write"],
      root: `/tmp/ocg-${scenario.version}-run`,
      exportSchema: () => generatedSchema,
      compileSchema: async () => "export interface ControlRevision { revision: number; }\n",
      writeText: (path, contents) => writes.push({ path: path.replaceAll("\\", "/"), contents }),
    });
    assert.deepEqual(
      writes.map((entry) => entry.path),
      [
        `/tmp/ocg-${scenario.version}-run/${scenario.schemaRelativePath}`,
        `/tmp/ocg-${scenario.version}-run/${scenario.typesRelativePath}`,
      ],
    );
  });
}

test("parseArgs stays shared across versions", () => {
  assert.equal(parseArgs(["--write"]), "write");
  assert.equal(parseArgs(["--check"]), "check");
  assert.throws(() => parseArgs([]), /exactly one/);
});
