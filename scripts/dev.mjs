import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

export const DEFAULT_DEV_GATEWAY_PORT = "19042";

export function devEnvironment(source = process.env, platform = process.platform) {
  const env = {
    ...source,
    OCG_GATEWAY_PORT: source.OCG_GATEWAY_PORT?.trim() || DEFAULT_DEV_GATEWAY_PORT,
    OCG_DEBUG_REQUESTS: source.OCG_DEBUG_REQUESTS?.trim() || "1",
    OCG_DEBUG_DIR: source.OCG_DEBUG_DIR?.trim() || fileURLToPath(new URL("../.artifacts/debug-requests", import.meta.url)),
    RUST_LOG: source.RUST_LOG?.trim() || "warn,ocg=debug",
  };
  // Windows environment keys are case-insensitive, but Node child spawning
  // sorts duplicate PATH/Path keys and may discard pnpm's local bin entries.
  if (platform === "win32") {
    const pathKey = Object.keys(source).find(key => key === "Path")
      ?? Object.keys(source).find(key => key.toLowerCase() === "path");
    if (pathKey) {
      const searchPath = source[pathKey];
      for (const key of Object.keys(env)) {
        if (key.toLowerCase() === "path") delete env[key];
      }
      env.Path = searchPath;
    }
  }
  return env;
}

/// Extra arguments after the script name are forwarded to the Tauri CLI, so
/// `pnpm run dev -- --no-watch` disables the Rust watcher while keeping this
/// script's port, logging, and request-capture environment.
export function tauriDevArgs(argv = process.argv.slice(2)) {
  return ["dev", ...argv];
}

const isMain = process.argv[1]
  && fileURLToPath(import.meta.url).toLowerCase() === process.argv[1].toLowerCase();

if (isMain) {
  const tauriCli = fileURLToPath(new URL("../node_modules/@tauri-apps/cli/tauri.js", import.meta.url));
  const env = devEnvironment();
  console.log(`Gateway development port: ${env.OCG_GATEWAY_PORT}`);
  console.log(`Process log filter: ${env.RUST_LOG}`);
  console.log(`Request capture: ${env.OCG_DEBUG_REQUESTS === "1" ? env.OCG_DEBUG_DIR : "disabled"}`);

  const child = spawn(process.execPath, [tauriCli, ...tauriDevArgs()], {
    cwd: process.cwd(),
    env,
    stdio: "inherit",
    windowsHide: false,
  });

  for (const signal of ["SIGINT", "SIGTERM"]) {
    process.once(signal, () => {
      if (!child.killed) child.kill(signal);
    });
  }

  child.once("error", (error) => {
    console.error(`Failed to start Tauri development mode: ${error.message}`);
    process.exitCode = 1;
  });
  child.once("exit", (code, signal) => {
    process.exitCode = code ?? (signal === "SIGINT" ? 130 : 1);
  });
}
