import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { writeUpdaterManifest } from "./generate-updater-manifest.mjs";
import { verifyReleaseAssets } from "./verify-release-assets.mjs";

const VERSION = "1.4.2";
const TAG = `v${VERSION}`;
const REPOSITORY = "klarkxy/open-console-gateway";

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

async function withReleaseFixture(callback) {
  const directory = mkdtempSync(join(tmpdir(), "ocg-release-assets-"));
  const names = [
    `ocg-manager_${VERSION}_windows-x64-setup.exe`,
    `ocg-manager_${VERSION}_windows-x64-setup.exe.sig`,
    `ocg-manager-cli_${VERSION}_windows-x64.zip`,
    `ocg-manager_${VERSION}_macos-universal.dmg`,
    `ocg-manager_${VERSION}_macos-universal.app.tar.gz`,
    `ocg-manager_${VERSION}_macos-universal.app.tar.gz.sig`,
    `ocg-manager-cli_${VERSION}_macos-universal.tar.gz`,
    `ocg-manager_${VERSION}_linux-x64.AppImage`,
    `ocg-manager_${VERSION}_linux-x64.AppImage.sig`,
    `ocg-manager_${VERSION}_linux-x64.deb`,
    `ocg-manager_${VERSION}_linux-x64.deb.sig`,
    `ocg-manager-cli_${VERSION}_linux-x64.tar.gz`,
    "compose.example.yaml",
    "cpa-config.example.yaml",
  ];
  for (const [index, name] of names.entries()) {
    writeFileSync(join(directory, name), name.endsWith(".sig") ? `signature-${index}\n` : `payload-${index}`);
  }
  writeUpdaterManifest({ releaseDir: directory, tag: TAG, repository: REPOSITORY });
  const checksumNames = [...names, "latest.json"].sort();
  writeFileSync(
    join(directory, "SHA256SUMS"),
    `${checksumNames.map((name) => `${sha256(join(directory, name))}  ${name}`).join("\n")}\n`,
  );
  const allNames = [...checksumNames, "SHA256SUMS"].sort();
  const metadata = allNames.map((name) => ({
    digest: `sha256:${sha256(join(directory, name))}`,
    name,
  }));
  const metadataPath = join(directory, "..", `${directory.split(/[\\/]/).at(-1)}-metadata.json`);
  writeFileSync(metadataPath, JSON.stringify(metadata));

  try {
    return await callback({ directory, metadataPath });
  } finally {
    rmSync(directory, { recursive: true, force: true });
    rmSync(metadataPath, { force: true });
  }
}

test("release verification accepts the exact manifest, checksum, and GitHub digest set", async () => {
  await withReleaseFixture(async ({ directory, metadataPath }) => {
    const result = await verifyReleaseAssets({
      releaseDir: directory,
      tag: TAG,
      repository: REPOSITORY,
      assetMetadataPath: metadataPath,
    });
    assert.deepEqual(result, { assetCount: 16, version: VERSION });
  });
});

test("release verification rejects payload and server digest drift", async () => {
  await withReleaseFixture(async ({ directory, metadataPath }) => {
    writeFileSync(join(directory, `ocg-manager_${VERSION}_linux-x64.deb`), "tampered");
    await assert.rejects(
      verifyReleaseAssets({ releaseDir: directory, tag: TAG, repository: REPOSITORY }),
      /SHA-256 mismatch/,
    );
    await assert.rejects(
      verifyReleaseAssets({
        releaseDir: directory,
        tag: TAG,
        repository: REPOSITORY,
        assetMetadataPath: metadataPath,
      }),
      /SHA-256 mismatch|digest mismatch/,
    );
  });
});

test("release verification rejects missing assets", async () => {
  await withReleaseFixture(async ({ directory }) => {
    rmSync(join(directory, `ocg-manager-cli_${VERSION}_windows-x64.zip`));
    await assert.rejects(
      verifyReleaseAssets({ releaseDir: directory, tag: TAG, repository: REPOSITORY }),
      /asset set mismatch/,
    );
  });
});
