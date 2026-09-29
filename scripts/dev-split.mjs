import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { devEnvironment } from "./dev.mjs";

export const VITE_DEV_PORT = process.env.OCG_VITE_PORT?.trim() || "30001";

export function splitEnvironment(source = process.env) {
  return {
    ...devEnvironment(source),
    OCG_DEV_DATA_DIR:
      source.OCG_DEV_DATA_DIR?.trim() ||
      fileURLToPath(new URL("../tmp/dev-data", import.meta.url)),
  };
}

export function cliBinaryRelativePath(platform = process.platform) {
  return platform === "win32"
    ? path.join("target", "debug", "ocg-manager-cli.exe")
    : path.join("target", "debug", "ocg-manager-cli");
}

export async function assertPortFree(port, label) {
  const free = await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", (error) => {
      if (error.code === "EADDRINUSE") {
        resolve(false);
        return;
      }
      reject(error);
    });
    server.once("listening", () => {
      server.close(() => resolve(true));
    });
    server.listen(Number(port), "127.0.0.1");
  });
  if (!free) {
    throw new Error(
      `${label} port ${port} is already in use; stop the process holding it ` +
        "(another dev instance or the installed app) or choose a different port",
    );
  }
}

const isMain = process.argv[1]
  && fileURLToPath(import.meta.url).toLowerCase() === process.argv[1].toLowerCase();

if (isMain) {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const env = splitEnvironment();
  const gatewayPort = env.OCG_GATEWAY_PORT;
  const dataDir = env.OCG_DEV_DATA_DIR;
  const cliBinary = path.join(root, cliBinaryRelativePath());

  console.log(`Gateway development port: ${gatewayPort}`);
  console.log(`Gateway data directory: ${dataDir}`);
  console.log(`Runtime log level: ${env.OCG_LOG_LEVEL}`);
  console.log(`Request capture: ${env.OCG_DEBUG_REQUESTS === "1" ? env.OCG_DEBUG_DIR : "disabled"}`);

  try {
    await assertPortFree(gatewayPort, "Gateway");
    await assertPortFree(VITE_DEV_PORT, "Vite dev server");
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }

  // Build once, then spawn the binary directly: killing a `cargo run` wrapper
  // would leave the actual server behind, and nothing here watches Rust
  // sources. Stop the script and rerun it to pick up Rust changes.
  const build = spawnSync("cargo", ["build", "-p", "ocg-manager-cli"], {
    cwd: root,
    env,
    stdio: "inherit",
  });
  if (build.status !== 0) {
    console.error("cargo build -p ocg-manager-cli failed; fix the build and rerun dev:split");
    process.exit(build.status ?? 1);
  }

  const firstRun = !existsSync(path.join(dataDir, "data.sqlite"));

  const children = [
    {
      name: "gateway",
      child: spawn(cliBinary, ["--data-dir", dataDir, "serve", "--port", gatewayPort], {
        cwd: root,
        env,
        stdio: "inherit",
      }),
    },
    {
      name: "vite",
      child: spawn(
        process.execPath,
        [fileURLToPath(new URL("../node_modules/vite/bin/vite.js", import.meta.url))],
        { cwd: root, env, stdio: "inherit" },
      ),
    },
  ];

  console.log(`Dashboard: http://127.0.0.1:${VITE_DEV_PORT}/dashboard/`);
  if (firstRun) {
    console.log(
      "First run with a fresh data directory. In a private terminal, retrieve the " +
        `development Gateway Key with: ${cliBinary} --data-dir ${dataDir} status --show-key`,
    );
  }

  let shuttingDown = false;
  let exitCode = 0;

  function shutdown(signal) {
    if (shuttingDown) return;
    shuttingDown = true;
    for (const { child } of children) {
      if (!child.killed) child.kill(signal);
    }
  }

  function maybeExit() {
    if (children.every(({ child }) => child.exitCode !== null || child.signalCode !== null)) {
      process.exit(exitCode);
    }
  }

  for (const { name, child } of children) {
    child.once("error", (error) => {
      console.error(`Failed to start ${name}: ${error.message}`);
      exitCode = 1;
      shutdown();
    });
    child.once("exit", (code, signal) => {
      if (!shuttingDown) {
        console.error(`${name} exited (${signal ?? code}); stopping the remaining process`);
        exitCode = code ?? 1;
        shutdown();
      }
      maybeExit();
    });
  }

  for (const signal of ["SIGINT", "SIGTERM"]) {
    process.once(signal, () => shutdown(signal));
  }
}
