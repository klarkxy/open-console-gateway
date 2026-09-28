import { createHash } from "node:crypto";
import {
  cpSync,
  createReadStream,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import {
  resolveFileSignerEnvironment,
  resolveMacosBundleTargets,
  resolveUpdaterBuildPlan,
  verifyUpdaterPublicKeyContinuity,
  verifyUpdaterSignature,
} from "./generate-updater-manifest.mjs";
import { pruneAppImageWaylandLibs } from "./prune-appimage-wayland-libs.mjs";
import { validateComposeVersion } from "./release-policy.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const targetDir = resolve(root, process.env.CARGO_TARGET_DIR ?? "target");
const releaseDir = join(root, "release");
const workDir = join(root, `.release-tmp-${process.pid}`);
const stagedReleaseDir = join(workDir, "release");
const cliPackageDir = join(workDir, "cli");
const updaterConfigPath = join(root, "src-tauri", "tauri.updater.conf.json");
const updaterPublicKeyFingerprintPath = join(root, "src-tauri", "updater-public-key.sha256");
const checkOnly = process.argv.length === 3 && process.argv[2] === "--check";

let workDirPrepared = false;

process.chdir(root);

function fail(message) {
  throw new Error(message);
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function tomlSection(source, name) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const header = new RegExp(`^\\[${escaped}\\][ \\t]*\\r?$`, "m").exec(source);
  if (!header) return "";
  const rest = source.slice(header.index + header[0].length).replace(/^\r?\n/, "");
  const nextSection = /^\[/m.exec(rest);
  return nextSection ? rest.slice(0, nextSection.index) : rest;
}

function tomlString(section, key) {
  const escaped = key.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return section.match(new RegExp(`^${escaped}\\s*=\\s*\"([^\"]+)\"`, "m"))?.[1];
}

function validateVersion() {
  const packageVersion = readJson(join(root, "package.json")).version;
  const tauriVersion = readJson(join(root, "src-tauri", "tauri.conf.json")).version;
  const cargo = readFileSync(join(root, "Cargo.toml"), "utf8");
  const cargoVersion = tomlString(tomlSection(cargo, "workspace.package"), "version");
  const appCargo = readFileSync(join(root, "src-tauri", "Cargo.toml"), "utf8");
  const appPackage = tomlSection(appCargo, "package");
  const appVersion = tomlString(appPackage, "version")
    ?? (/^version\.workspace\s*=\s*true\s*$/m.test(appPackage) ? cargoVersion : undefined);
  const versions = new Set([packageVersion, tauriVersion, cargoVersion, appVersion]);

  if ([...versions].some((version) => !version) || versions.size !== 1) {
    fail(
      `Version mismatch: package.json=${packageVersion ?? "missing"}, `
      + `tauri.conf.json=${tauriVersion ?? "missing"}, workspace=${cargoVersion ?? "missing"}, `
      + `src-tauri=${appVersion ?? "missing"}`,
    );
  }

  const releaseTag = process.env.OCG_RELEASE_TAG?.trim() || (process.env.GITHUB_REF_TYPE === "tag"
    ? process.env.GITHUB_REF_NAME
    : process.env.GITHUB_REF?.startsWith("refs/tags/")
      ? process.env.GITHUB_REF.slice("refs/tags/".length)
      : undefined);
  if (releaseTag && releaseTag !== packageVersion && releaseTag !== `v${packageVersion}`) {
    fail(`Release tag ${releaseTag} does not match version ${packageVersion}.`);
  }

  validateComposeVersion(readFileSync(join(root, "compose.example.yaml"), "utf8"), packageVersion);

  return packageVersion;
}

function validateUpdaterPublicKey(plan) {
  if (!plan.publicKey) return;
  verifyUpdaterPublicKeyContinuity({
    publicKey: plan.publicKey,
    expectedFingerprint: readFileSync(updaterPublicKeyFingerprintPath, "utf8"),
  });
}

function hostPlatform() {
  if (process.platform === "win32" && process.arch === "x64") return "windows-x64";
  if (process.platform === "linux" && process.arch === "x64") return "linux-x64";
  if (process.platform === "darwin" && ["x64", "arm64"].includes(process.arch)) {
    return "macos-universal";
  }
  fail(`Unsupported release host: ${process.platform}/${process.arch}.`);
}

function run(command, args, options = {}) {
  console.log(`> ${basename(command)} ${args.join(" ")}`);
  const result = spawnSync(command, args, {
    cwd: root,
    stdio: "inherit",
    ...options,
  });
  if (result.error) fail(`${command} failed: ${result.error.message}`);
  if (result.status !== 0) fail(`${command} exited with status ${result.status}.`);
}

function onlyArtifact(directory, suffix, label) {
  if (!existsSync(directory)) fail(`Missing ${label} directory: ${directory}`);
  const matches = readdirSync(directory, { withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(suffix))
    .map((entry) => join(directory, entry.name));
  if (matches.length !== 1) {
    fail(`Expected one newly built ${label} in ${directory}, found ${matches.length}.`);
  }
  return matches[0];
}

function requireFile(path, label) {
  if (!existsSync(path)) fail(`Missing ${label}: ${path}`);
  return path;
}

function stageArtifact(source, name, artifacts) {
  cpSync(requireFile(source, name), join(stagedReleaseDir, name));
  artifacts.push(name);
}

function stageUpdaterArtifact(source, name, artifacts, publicKey) {
  verifyUpdaterSignature({
    payloadPath: source,
    signaturePath: `${source}.sig`,
    publicKey,
  });
  stageArtifact(source, name, artifacts);
  stageArtifact(`${source}.sig`, `${name}.sig`, artifacts);
}

function updaterBuildArgs(plan, secretConfigPath) {
  if (!plan.enabled) return [];
  return ["--config", updaterConfigPath, "--config", secretConfigPath];
}

function verifyUpdaterSigningPair(plan, tauriCli) {
  if (!plan.enabled) return;
  const payloadPath = join(workDir, "updater-signing-preflight.txt");
  writeFileSync(payloadPath, "Open Console Gateway updater signing preflight\n");
  run(process.execPath, [tauriCli, "signer", "sign", payloadPath], {
    env: resolveFileSignerEnvironment(),
  });
  verifyUpdaterSignature({
    payloadPath,
    signaturePath: `${payloadPath}.sig`,
    publicKey: plan.publicKey,
  });
}

function prepareCliPackage(binary) {
  const cliName = process.platform === "win32" ? "ocg-manager-cli.exe" : "ocg-manager-cli";
  cpSync(requireFile(binary, "CLI binary"), join(cliPackageDir, cliName));
  cpSync(requireFile(join(root, "LICENSE"), "LICENSE"), join(cliPackageDir, "LICENSE"));
  requireFile(join(root, "dist", "index.html"), "dashboard dist");
  cpSync(join(root, "dist"), join(cliPackageDir, "dist"), { recursive: true, force: true });
  requireFile(join(root, "resources", "codex-byok", "prompt.md"), "Codex BYOK prompt");
  cpSync(join(root, "resources", "codex-byok"), join(cliPackageDir, "licenses", "codex"), {
    recursive: true,
    force: true,
  });
}

function archiveCli(platform, output) {
  if (platform === "windows-x64") {
    run(
      "powershell.exe",
      [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        "$ProgressPreference = 'SilentlyContinue'; Compress-Archive -Path (Join-Path $env:OCG_CLI_PACKAGE '*') -DestinationPath $env:OCG_CLI_ARCHIVE -CompressionLevel Optimal -Force",
      ],
      {
        env: {
          ...process.env,
          OCG_CLI_PACKAGE: cliPackageDir,
          OCG_CLI_ARCHIVE: output,
        },
      },
    );
    return;
  }
  run("tar", ["-czf", output, "-C", cliPackageDir, "ocg-manager-cli", "dist", "LICENSE", "licenses"]);
}

async function sha256(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

async function writeChecksums(artifacts) {
  const lines = [];
  for (const name of [...artifacts].sort()) {
    lines.push(`${await sha256(join(stagedReleaseDir, name))}  ${name}`);
  }
  writeFileSync(join(stagedReleaseDir, "SHA256SUMS"), `${lines.join("\n")}\n`);
}

function replaceRelease() {
  const backupDir = join(root, `.release-backup-${process.pid}`);
  rmSync(backupDir, { recursive: true, force: true });
  if (existsSync(releaseDir)) renameSync(releaseDir, backupDir);
  try {
    renameSync(stagedReleaseDir, releaseDir);
    rmSync(backupDir, { recursive: true, force: true });
  } catch (error) {
    if (existsSync(releaseDir)) rmSync(releaseDir, { recursive: true, force: true });
    if (existsSync(backupDir)) renameSync(backupDir, releaseDir);
    throw error;
  }
}

async function main() {
  if (process.argv.length > 2 && !checkOnly) {
    fail("Usage: release.mjs [--check]");
  }
  const version = validateVersion();
  const updaterPlan = resolveUpdaterBuildPlan();
  validateUpdaterPublicKey(updaterPlan);
  if (checkOnly) {
    rmSync(workDir, { recursive: true, force: true });
    workDirPrepared = true;
    mkdirSync(workDir, { recursive: true });
    if (updaterPlan.enabled) {
      const tauriCli = fileURLToPath(import.meta.resolve("@tauri-apps/cli/tauri.js"));
      verifyUpdaterSigningPair(updaterPlan, tauriCli);
    }
    console.log(
      `Release preflight passed for v${version} (${updaterPlan.enabled ? "signed updater" : "unsigned local"}).`,
    );
    return;
  }

  const tauriCli = fileURLToPath(import.meta.resolve("@tauri-apps/cli/tauri.js"));
  const platform = hostPlatform();
  const artifacts = [];
  const tauriBuildEnvironment = { ...process.env };
  delete tauriBuildEnvironment.TAURI_SIGNING_PRIVATE_KEY_PATH;

  if (!updaterPlan.enabled) {
    console.warn(
      "Updater signing key not configured; building ordinary local artifacts only. "
      + "This output is suitable for local smoke testing, not an updater-enabled published release.",
    );
  }

  rmSync(workDir, { recursive: true, force: true });
  workDirPrepared = true;
  mkdirSync(stagedReleaseDir, { recursive: true });
  mkdirSync(cliPackageDir, { recursive: true });

  let secretConfigPath;
  if (updaterPlan.enabled) {
    secretConfigPath = join(workDir, "tauri.updater.secrets.conf.json");
    writeFileSync(
      secretConfigPath,
      `${JSON.stringify({ plugins: { updater: { pubkey: updaterPlan.publicKey } } }, null, 2)}\n`,
      { mode: 0o600 },
    );
    console.log(`Updater artifacts enabled with ${updaterPlan.privateKeySource}.`);
  }
  const buildConfigArgs = updaterBuildArgs(updaterPlan, secretConfigPath);

  if (platform === "windows-x64") {
    const bundleDir = join(targetDir, "release", "bundle", "nsis");
    rmSync(bundleDir, { recursive: true, force: true });
    run(process.execPath, [
      tauriCli,
      "build",
      "--ci",
      "--bundles",
      "nsis",
      ...buildConfigArgs,
    ], { env: tauriBuildEnvironment });
    const installerName = `ocg-manager_${version}_windows-x64-setup.exe`;
    const installer = onlyArtifact(bundleDir, "-setup.exe", "NSIS installer");
    if (updaterPlan.enabled) {
      stageUpdaterArtifact(installer, installerName, artifacts, updaterPlan.publicKey);
    }
    else stageArtifact(installer, installerName, artifacts);

    const cliBinary = join(targetDir, "release", "ocg-manager-cli.exe");
    rmSync(cliBinary, { force: true });
    run("cargo", ["build", "--release", "--bin", "ocg-manager-cli"]);
    prepareCliPackage(cliBinary);
    const archiveName = `ocg-manager-cli_${version}_windows-x64.zip`;
    archiveCli(platform, join(stagedReleaseDir, archiveName));
    artifacts.push(archiveName);
  } else if (platform === "linux-x64") {
    const bundleRoot = join(targetDir, "release", "bundle");
    rmSync(join(bundleRoot, "appimage"), { recursive: true, force: true });
    rmSync(join(bundleRoot, "deb"), { recursive: true, force: true });
    run(process.execPath, [
      tauriCli,
      "build",
      "--ci",
      "--bundles",
      "appimage,deb",
      ...buildConfigArgs,
    ], { env: tauriBuildEnvironment });
    const appImage = onlyArtifact(join(bundleRoot, "appimage"), ".AppImage", "AppImage");
    const prunedAppImage = pruneAppImageWaylandLibs(appImage, { failClosed: true });
    const appImageName = `ocg-manager_${version}_linux-x64.AppImage`;
    if (updaterPlan.enabled) {
      rmSync(`${prunedAppImage}.sig`, { force: true });
      run(process.execPath, [tauriCli, "signer", "sign", prunedAppImage], {
        env: resolveFileSignerEnvironment(),
      });
      stageUpdaterArtifact(prunedAppImage, appImageName, artifacts, updaterPlan.publicKey);
    }
    else stageArtifact(prunedAppImage, appImageName, artifacts);

    const deb = onlyArtifact(join(bundleRoot, "deb"), ".deb", "deb package");
    const debName = `ocg-manager_${version}_linux-x64.deb`;
    if (updaterPlan.enabled) {
      rmSync(`${deb}.sig`, { force: true });
      run(process.execPath, [tauriCli, "signer", "sign", deb], {
        env: resolveFileSignerEnvironment(),
      });
      stageUpdaterArtifact(deb, debName, artifacts, updaterPlan.publicKey);
    } else {
      stageArtifact(deb, debName, artifacts);
    }

    const cliBinary = join(targetDir, "release", "ocg-manager-cli");
    rmSync(cliBinary, { force: true });
    run("cargo", ["build", "--release", "--bin", "ocg-manager-cli"]);
    prepareCliPackage(cliBinary);
    const archiveName = `ocg-manager-cli_${version}_linux-x64.tar.gz`;
    archiveCli(platform, join(stagedReleaseDir, archiveName));
    artifacts.push(archiveName);
  } else {
    const universalTarget = join(targetDir, "universal-apple-darwin", "release");
    const dmgDir = join(universalTarget, "bundle", "dmg");
    const macosDir = join(universalTarget, "bundle", "macos");
    rmSync(dmgDir, { recursive: true, force: true });
    if (updaterPlan.enabled) rmSync(macosDir, { recursive: true, force: true });
    run(process.execPath, [
      tauriCli,
      "build",
      "--ci",
      "--target",
      "universal-apple-darwin",
      "--bundles",
      resolveMacosBundleTargets(updaterPlan.enabled),
      ...buildConfigArgs,
    ], { env: tauriBuildEnvironment });
    stageArtifact(
      onlyArtifact(dmgDir, ".dmg", "universal DMG"),
      `ocg-manager_${version}_macos-universal.dmg`,
      artifacts,
    );
    if (updaterPlan.enabled) {
      stageUpdaterArtifact(
        onlyArtifact(macosDir, ".app.tar.gz", "universal updater archive"),
        `ocg-manager_${version}_macos-universal.app.tar.gz`,
        artifacts,
        updaterPlan.publicKey,
      );
    }

    const x64Cli = join(targetDir, "x86_64-apple-darwin", "release", "ocg-manager-cli");
    const arm64Cli = join(targetDir, "aarch64-apple-darwin", "release", "ocg-manager-cli");
    rmSync(x64Cli, { force: true });
    rmSync(arm64Cli, { force: true });
    run("cargo", ["build", "--release", "--bin", "ocg-manager-cli", "--target", "x86_64-apple-darwin"]);
    run("cargo", ["build", "--release", "--bin", "ocg-manager-cli", "--target", "aarch64-apple-darwin"]);
    const universalCli = join(workDir, "ocg-manager-cli");
    run("lipo", ["-create", x64Cli, arm64Cli, "-output", universalCli]);
    run("codesign", ["--force", "--sign", "-", "--timestamp=none", universalCli]);
    prepareCliPackage(universalCli);
    const archiveName = `ocg-manager-cli_${version}_macos-universal.tar.gz`;
    archiveCli(platform, join(stagedReleaseDir, archiveName));
    artifacts.push(archiveName);
  }

  await writeChecksums(artifacts);
  replaceRelease();
  console.log(`Release ready: ${releaseDir}`);
}

try {
  await main();
} catch (error) {
  console.error(`Release failed; existing release/ was preserved.\n${error.stack ?? error}`);
  process.exitCode = 1;
} finally {
  if (workDirPrepared) rmSync(workDir, { recursive: true, force: true });
}
