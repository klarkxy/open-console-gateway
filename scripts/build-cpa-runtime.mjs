import { createHash, randomBytes } from "node:crypto";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const orch = path.join(root, "tmp", "ocg3-cli-delivery", "orchestration-20261004");
const source = path.join(orch, "cpa-source");
const build = path.join(orch, "runtime-build");
const hostSrc = path.join(root, "runtime", "cpa");
const hostDst = path.join(build, "host");
const trees = path.join(build, "trees");

const args = process.argv.slice(2);
let fixture = false;
let outputDirArg = "";
for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg === "--native-loopback-fixture") {
    fixture = true;
    continue;
  }
  if (arg === "--output-dir" || arg.startsWith("--output-dir=")) {
    outputDirArg = arg === "--output-dir" ? args[index + 1] : arg.slice("--output-dir=".length);
    if (arg === "--output-dir") {
      index += 1;
    }
    if (!outputDirArg) {
      throw new Error("missing --output-dir value");
    }
    continue;
  }
  throw new Error(`unknown flag ${arg}`);
}
const output = resolveOwnedOutput(outputDirArg || (fixture ? path.join(build, "native-loopback-fixture") : build));
const variant = fixture ? "native-loopback-fixture" : "production";
const buildTags = fixture ? ["ocg_native_loopback_fixture"] : [];
const tagArgs = fixture ? ["-tags", "ocg_native_loopback_fixture"] : [];
const tagText = fixture ? "-tags ocg_native_loopback_fixture " : "";

const pins = JSON.parse(fs.readFileSync(path.join(hostSrc, "pins.json"), "utf8"));
for (const [rel, want] of Object.entries(pins.files)) {
  const got = sha256(fs.readFileSync(path.join(source, rel)));
  if (got !== want) {
    throw new Error(`pin mismatch ${rel}`);
  }
}

const patches = fs.readdirSync(path.join(hostSrc, "patches")).filter((name) => name.endsWith(".patch")).sort();
const patchLines = patches.map((name) => {
  const digest = sha256(fs.readFileSync(path.join(hostSrc, "patches", name)));
  return `patch ${name} ${digest}`;
});
const overlayFiles = listFiles(path.join(hostSrc, "overlay"), () => true);
const overlayLines = overlayFiles.map((rel) => `overlay ${rel} ${sha256(fs.readFileSync(path.join(hostSrc, "overlay", rel)))}`);
const hostFiles = listFiles(hostSrc, hostInput);
const hostLines = hostFiles.map((rel) => `host ${rel} ${sha256(fs.readFileSync(path.join(hostSrc, rel)))}`);
const identityText = [
  `commit ${pins.commit}`,
  `version ${pins.version}`,
  ...patchLines,
  ...overlayLines,
  ...hostLines,
].join("\n") + "\n";
const buildIdentity = sha256(Buffer.from(identityText));
const hostSHA256 = sha256(Buffer.from(hostLines.join("\n") + "\n"));
const overlaySHA256 = sha256(Buffer.from(overlayLines.join("\n") + "\n"));
const tree = ensureTree(buildIdentity);

fs.mkdirSync(hostDst, { recursive: true });
const copied = new Set();
for (const name of fs.readdirSync(hostSrc)) {
  if (name.endsWith(".go") || name === "go.mod" || name === "capabilities.json" || name === "policy-v1.schema.json") {
    fs.copyFileSync(path.join(hostSrc, name), path.join(hostDst, name));
    copied.add(name);
  }
}
fs.cpSync(path.join(hostSrc, "testdata"), path.join(hostDst, "testdata"), { recursive: true });
for (const name of fs.readdirSync(hostDst)) {
  if (name.endsWith(".go") && !copied.has(name)) {
    fs.rmSync(path.join(hostDst, name));
  }
}
const goMod = fs.readFileSync(path.join(hostDst, "go.mod"), "utf8");
const replaced = goMod.replace(
  /replace github.com\/router-for-me\/CLIProxyAPI\/v8 =>[^\r\n]*/,
  `replace github.com/router-for-me/CLIProxyAPI/v8 => ../trees/${buildIdentity}`,
);
if (replaced === goMod || !replaced.includes(`../trees/${buildIdentity}`)) {
  throw new Error("copied host go.mod replace was not retargeted");
}
fs.writeFileSync(path.join(hostDst, "go.mod"), replaced);

const env = { ...process.env, GOCACHE: path.join(build, "gocache"), GOMODCACHE: path.join(build, "gomodcache"), GOTOOLCHAIN: "auto", CGO_ENABLED: "0" };
delete env.GOSUMDB;
delete env.GOFLAGS;
fs.mkdirSync(env.GOCACHE, { recursive: true });
fs.mkdirSync(env.GOMODCACHE, { recursive: true });
fs.mkdirSync(output, { recursive: true });
run("go", ["mod", "tidy"], hostDst, env);
const exe = path.join(output, "ocg-cpa-host.exe");
const goBuildFlags = ["-trimpath", "-buildvcs=false"];
run("go", ["build", ...goBuildFlags, ...tagArgs, "-o", exe, "."], hostDst, env);
const testEnv = { ...env, OCG_CPA_HOST_EXE: exe };
run("go", ["test", ...tagArgs, "-p", "1", "-count=1", "-timeout", "15m"], hostDst, testEnv);
const dispatchGuardCommand = `go test ${tagText}-count=1 -timeout 15m -run TestOCGNativeDispatchGuard ./sdk/cliproxy/auth`;
run("go", ["test", ...tagArgs, "-count=1", "-timeout", "15m", "-run", "TestOCGNativeDispatchGuard", "./sdk/cliproxy/auth"], tree, testEnv);
const candidates = fixture ? [] : [
  buildCandidate("linux", "amd64", "ocg-cpa-host-linux-amd64"),
  buildCandidate("darwin", "arm64", "ocg-cpa-host-darwin-arm64"),
];

const caps = JSON.parse(fs.readFileSync(path.join(hostSrc, "capabilities.json"), "utf8"));
if (caps.capabilities.some((name) => String(name).toLowerCase().includes("websocket"))) {
  throw new Error("websocket is not a claimed capability");
}
const sendCoverage = collectSendCoverage(tree, hostSrc);
if (!fixture) {
  const trace = path.join(build, "policy-trace.log");
  if (fs.existsSync(trace)) {
    fs.rmSync(trace);
  }
}
const goos = runCapture("go", ["env", "GOOS"], hostDst, env);
const goarch = runCapture("go", ["env", "GOARCH"], hostDst, env);
const exeRel = orchPath(exe);
const wrapper = ["node scripts/build-cpa-runtime.mjs", ...(fixture ? ["--native-loopback-fixture"] : []), ...(outputDirArg ? ["--output-dir", outputDirArg] : [])].join(" ");
const executableName = path.basename(exe);
if (executableName !== "ocg-cpa-host.exe" || executableName.includes("/") || executableName.includes("\\")) {
  throw new Error(`host executable basename is not ocg-cpa-host.exe: ${executableName}`);
}
const manifest = {
  variant,
  buildTags,
  os: normalizePlatformOS(goos),
  arch: normalizePlatformArch(goarch),
  executable: executableName,
  sourceCommit: pins.commit,
  sourceVersion: pins.version,
  executableSHA256: sha256(fs.readFileSync(exe)),
  buildIdentity,
  hostSHA256,
  overlaySHA256,
  protocolVersion: caps.protocolVersion,
  capabilities: caps.capabilities,
  fullCLIAccepted: false,
  build: {
    command: `go build ${tagText}-trimpath -buildvcs=false -o ${exeRel} .`,
    wrapper,
    goos,
    goarch,
    env: {
      GOTOOLCHAIN: "auto",
      CGO_ENABLED: "0",
      GOCACHE: "runtime-build/gocache",
      GOMODCACHE: "runtime-build/gomodcache",
    },
    test: {
      command: `go test ${tagText}-p 1 -count=1 -timeout 15m`,
      directory: "runtime-build/host",
      executable: exeRel,
    },
    nativeDispatchGuard: {
      command: dispatchGuardCommand,
      directory: `runtime-build/trees/${buildIdentity}`,
      result: "passed",
    },
    candidates,
    note: fixture
      ? "The Windows executable ran the host suite and the native dispatch guard. No other platform was compiled. This is not full CLI acceptance."
      : "The Windows executable ran the host suite. linux/amd64 and darwin/arm64 were compiled only and were not interactively accepted. This is not full CLI acceptance.",
  },
  sendCoverage,
  patches: patches.map((name) => ({ name, sha256: sha256(fs.readFileSync(path.join(hostSrc, "patches", name))) })),
  pins: pins.files,
};
fs.writeFileSync(path.join(output, "manifest.json"), JSON.stringify(manifest, null, 2));
fs.writeFileSync(path.join(output, "send-coverage.json"), JSON.stringify(sendCoverage, null, 2));
process.stdout.write(`${manifest.executableSHA256} ${buildIdentity} ${manifest.patches.length} patches\n`);

function ensureTree(identity) {
  fs.mkdirSync(trees, { recursive: true });
  const finalDir = path.join(trees, identity);
  const markerName = ".ocg-build-identity";
  if (fs.existsSync(finalDir)) {
    const markerPath = path.join(finalDir, markerName);
    const marker = fs.existsSync(markerPath) ? fs.readFileSync(markerPath, "utf8").trim() : "";
    if (marker !== identity) {
      throw new Error(`build tree ${finalDir} exists without a matching identity marker`);
    }
    return finalDir;
  }
  const incomplete = path.join(trees, `${identity}.incomplete-${randomBytes(4).toString("hex")}`);
  fs.cpSync(source, incomplete, {
    recursive: true,
    filter: (src) => path.basename(src) !== ".git",
  });
  for (const name of patches) {
    const patch = fs.readFileSync(path.join(hostSrc, "patches", name));
    if (!applyPatch(incomplete, patch, name)) {
      throw new Error(`git apply failed for ${name}; left ${incomplete}`);
    }
  }
  fs.cpSync(path.join(hostSrc, "overlay"), incomplete, { recursive: true });
  fs.writeFileSync(path.join(incomplete, markerName), `${identity}\n`);
  fs.renameSync(incomplete, finalDir);
  return finalDir;
}

function applyPatch(cwd, patch, name) {
  const dummy = path.join(build, "dummy-git");
  if (!fs.existsSync(path.join(dummy, "HEAD"))) {
    spawnSync("git", ["init", dummy], { stdio: "ignore" });
  }
  const gitEnv = { ...process.env, GIT_DIR: dummy };
  delete gitEnv.GIT_WORK_TREE;
  const result = spawnSync("git", ["-c", "core.autocrlf=false", "apply", "--whitespace=nowarn", "-p2"], {
    cwd,
    env: gitEnv,
    input: patch,
  });
  if (result.status === 0) {
    return true;
  }
  process.stderr.write(`${name}\n`);
  process.stderr.write(result.stderr || "");
  return false;
}

function buildCandidate(goos, goarch, name) {
  const child = { ...env, GOOS: goos, GOARCH: goarch, CGO_ENABLED: "0" };
  delete child.GOFLAGS;
  const out = path.join(output, name);
  const command = `go build -trimpath -buildvcs=false -o ${orchPath(out)} .`;
  run("go", ["build", "-trimpath", "-buildvcs=false", "-o", out, "."], hostDst, child);
  const executable = path.basename(out);
  return {
    os: normalizePlatformOS(goos),
    arch: normalizePlatformArch(goarch),
    executable,
    goos,
    goarch,
    command,
    executableSHA256: sha256(fs.readFileSync(out)),
    acceptance: "compiled-only",
  };
}

function normalizePlatformOS(value) {
  switch (String(value).toLowerCase()) {
    case "windows":
    case "win32":
      return "windows";
    case "linux":
      return "linux";
    case "darwin":
    case "macos":
      return "macos";
    default:
      throw new Error(`unsupported GOOS ${value}`);
  }
}

function normalizePlatformArch(value) {
  switch (String(value).toLowerCase()) {
    case "amd64":
    case "x86_64":
    case "x64":
      return "x86_64";
    case "arm64":
    case "aarch64":
      return "aarch64";
    default:
      throw new Error(`unsupported GOARCH ${value}`);
  }
}

function resolveOwnedOutput(raw) {
  const resolved = path.resolve(raw);
  if (!fs.existsSync(build) || !fs.statSync(build).isDirectory()) {
    throw new Error("runtime-build is not a directory");
  }
  let existing = resolved;
  while (!fs.existsSync(existing)) {
    const parent = path.dirname(existing);
    if (parent === existing) {
      throw new Error(`output dir escapes runtime-build: ${resolved}`);
    }
    existing = parent;
  }
  const stat = fs.lstatSync(existing);
  if (!stat.isDirectory()) {
    throw new Error(`output ancestor is not a directory: ${existing}`);
  }
  const ancestorReal = fs.realpathSync(existing);
  const buildReal = fs.realpathSync(build);
  const suffix = path.relative(existing, resolved);
  if (suffix.split(/[\\/]/).includes("..")) {
    throw new Error(`output dir escapes runtime-build: ${resolved}`);
  }
  const destination = path.resolve(ancestorReal, suffix);
  const relative = path.relative(buildReal, destination);
  if (relative !== "" && (relative.startsWith("..") || path.isAbsolute(relative))) {
    throw new Error(`output dir escapes runtime-build: ${resolved}`);
  }
  return destination;
}

function orchPath(abs) {
  return path.relative(orch, abs).split(path.sep).join("/");
}

function run(cmd, args, cwd, childEnv) {
  process.stdout.write(`+ ${cmd} ${args.join(" ")}\n`);
  const result = spawnSync(cmd, args, { cwd, env: childEnv, stdio: "inherit", timeout: 900000 });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(`${cmd} ${args.join(" ")} failed`);
  }
}

function runCapture(cmd, args, cwd, childEnv) {
  const result = spawnSync(cmd, args, { cwd, env: childEnv, encoding: "utf8", timeout: 60000 });
  if (result.status !== 0) {
    throw new Error(`${cmd} ${args.join(" ")} failed`);
  }
  return result.stdout.trim();
}

function collectSendCoverage(tree, sourceHost) {
  const required = [
    ["sdk/cliproxy/auth/conductor_execution.go", "GenerationKindExecute"],
    ["sdk/cliproxy/auth/conductor_execution.go", "GenerationKindRefreshResend"],
    ["sdk/cliproxy/auth/conductor_execution.go", "GenerationKindCount"],
    ["sdk/cliproxy/auth/conductor_execution.go", "ensureRequestStop"],
    ["sdk/cliproxy/auth/conductor_home_execution.go", "GenerationKindCount"],
    ["sdk/cliproxy/auth/conductor_home.go", "beforeGenerationSend"],
    ["sdk/cliproxy/auth/conductor_stream.go", "GenerationKindStream"],
    ["sdk/cliproxy/auth/conductor_stream.go", "GenerationKindStreamRefresh"],
    ["sdk/cliproxy/auth/conductor_stream.go", "GenerationKindStreamBootstrap"],
    ["sdk/cliproxy/auth/conductor_stream.go", "recordCallerStop"],
    ["sdk/cliproxy/auth/conductor_cooldown.go", "!result.HaltRotation"],
    ["sdk/cliproxy/auth/conductor_refresh.go", "UpdateRefreshedAuth"],
    ["sdk/cliproxy/auth/conductor_lifecycle.go", "refreshOCGMaterial"],
    ["sdk/cliproxy/auth/conductor_lifecycle.go", "ocgRegistrationMaterialFence"],
    ["internal/runtime/executor/codex_executor_execute.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/codex_executor_execute.go", "NoteOCGHTTPDispatch"],
    ["internal/runtime/executor/codex_executor_stream.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/codex_executor_stream.go", "NoteOCGHTTPDispatch"],
    ["internal/runtime/executor/codex_executor_request.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/codex_openai_images.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/antigravity_executor_execute.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/antigravity_executor_execute.go", "NoteOCGHTTPDispatch"],
    ["internal/runtime/executor/antigravity_executor_stream.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/antigravity_executor_stream.go", "NoteOCGHTTPDispatch"],
    ["internal/runtime/executor/antigravity_executor_tokens.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/antigravity_executor_tokens.go", "NoteOCGHTTPDispatch"],
    ["internal/runtime/executor/antigravity_executor.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/antigravity_executor_execute.go", "PublishOCGAntigravityUsage"],
    ["internal/runtime/executor/antigravity_executor_stream.go", "PublishOCGAntigravityUsage"],
    ["internal/runtime/executor/claude_executor_request.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/claude_executor_request.go", "NoteOCGHTTPDispatch"],
    ["internal/runtime/executor/claude_executor.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/openai_compat_executor.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/openai_compat_executor.go", "NoteOCGUsagePayload"],
    ["internal/runtime/executor/kimi_executor.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/xai_executor.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/xai_executor_execute.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/xai_executor_stream.go", "BeforeOCGHTTPDispatch"],
    ["internal/runtime/executor/xai_executor_media.go", "BeforeOCGHTTPDispatch"],
    ["sdk/cliproxy/auth/conductor_execution.go", "NoteOCGSourceFormat"],
    ["sdk/cliproxy/auth/conductor_stream.go", "NoteOCGSourceFormat"],
    ["sdk/cliproxy/auth/conductor_home_execution.go", "NoteOCGSourceFormat"],
    ["sdk/cliproxy/auth/conductor_home.go", "NoteOCGSourceFormat"],
    ["internal/runtime/executor/antigravity_executor_execute.go", "CurrentAttemptID"],
    ["internal/runtime/executor/antigravity_executor_execute.go", "AdmitInternalGeneration"],
    ["internal/runtime/executor/codex_websockets_execute.go", "AdmitInternalGeneration"],
    ["internal/runtime/executor/codex_websockets_stream.go", "AdmitInternalGeneration"],
    ["internal/runtime/executor/openai_compat_ocg.go", "PublishInternalGeneration"],
    ["internal/runtime/executor/openai_compat_ocg.go", "AdmitInternalGeneration"],
    ["../runtime/cpa/host.go", "policyIngress"],
    ["../runtime/cpa/host.go", "/v1beta/interactions"],
    ["../runtime/cpa/host.go", `websockets"] = "false"`],
  ];
  const present = [];
  for (const [rel, marker] of required) {
    const file = rel.startsWith("../") ? path.join(sourceHost, path.basename(rel)) : path.join(tree, rel);
    const text = fs.readFileSync(file, "utf8");
    if (!text.includes(marker)) {
      throw new Error(`ungated send coverage missing ${marker} in ${rel}`);
    }
    present.push({ file: rel.replace("../runtime/cpa/", "runtime/cpa/"), marker });
  }
  return {
    websocket: "fail-closed-unclaimed",
    ingress: [
      "/v1/",
      "/openai/v1/",
      "/backend-api/codex/",
      "/v1beta/interactions",
      "/v1beta/models/*:generateContent",
      "/v1beta/models/*:streamGenerateContent",
      "/v1beta/models/*:countTokens",
    ],
    markers: present,
  };
}

function listFiles(dir, predicate) {
  const out = [];
  const walk = (current, rel) => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      if (entry.name === ".git") {
        continue;
      }
      const childRel = rel ? `${rel}/${entry.name}` : entry.name;
      const child = path.join(current, entry.name);
      if (entry.isDirectory()) {
        walk(child, childRel);
      } else if (predicate(childRel)) {
        out.push(childRel);
      }
    }
  };
  walk(dir, "");
  out.sort();
  return out;
}

function hostInput(rel) {
  if (rel.startsWith("internal/") || rel.startsWith("patches/") || rel.startsWith("overlay/")) {
    return false;
  }
  return rel.endsWith(".go") || rel === "go.mod" || rel === "capabilities.json" || rel === "policy-v1.schema.json" || rel === "pins.json" || rel.startsWith("testdata/");
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}
