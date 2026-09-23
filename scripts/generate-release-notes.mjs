import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const PLATFORM_RELEASE_NOTES =
  "Updater payloads include Tauri minisign signatures. The Windows installer remains unsigned by Authenticode; the macOS app uses ad-hoc signing and is not notarized.";

export const PRERELEASE_WARNING = `> [!WARNING]
> **Beta preview: managed account registration and isolated browser profiles are Beta and have not been thoroughly tested.**
>
> The following remain unverified in real release conditions:
> - Real Google signup, OpenCode signup, and payment flows.
> - noVNC keyboard and clipboard behavior.
> - Live GHCR first-publication behavior for the browser image.
>
> This preview also includes gateway, redaction, and release-pipeline changes. Do not treat it as production-ready.`;

const PRERELEASE_IDENTIFIER = "(?:0|[1-9]\\d*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)";
const VERSION_TAG = new RegExp(
  `^v?(0|[1-9]\\d*)\\.(0|[1-9]\\d*)\\.(0|[1-9]\\d*)(?:-${PRERELEASE_IDENTIFIER}(?:\\.${PRERELEASE_IDENTIFIER})*)?$`,
);
const CONVENTIONAL = /^(?:[\p{Emoji_Presentation}\p{Extended_Pictographic}\uFE0F\u200D]+\s*)*([a-zA-Z]+)(?:\(([^)]+)\))?(!)?:\s*(.+)$/u;
const EXCLUDED_TYPES = new Set(["style", "test"]);
const PREPARE_RELEASE = /^(?:release|chore)(?:\([^)]*\))?: (?:prepare|bump|release)\b/i;

const SECTIONS = Object.freeze([
  ["feat", "Features"],
  ["fix", "Fixes"],
  ["perf", "Performance"],
  ["refactor", "Refactors"],
  ["docs", "Documentation"],
  ["security", "Security"],
  ["build", "Build"],
  ["ci", "CI"],
  ["chore", "Maintenance"],
]);

const SECTION_TYPES = new Set(SECTIONS.map(([type]) => type));

export function normalizeTagName(value, label = "tag") {
  const raw = String(value ?? "").trim();
  const match = VERSION_TAG.exec(raw);
  if (!match) {
    throw new Error(`${label} must look like v1.5.7; received ${raw || "<empty>"}.`);
  }
  return `v${match[1]}.${match[2]}.${match[3]}${raw.includes("-") ? raw.slice(raw.indexOf("-")) : ""}`;
}

function normalizeTagNameLoose(value) {
  const raw = String(value ?? "").trim();
  if (!VERSION_TAG.test(raw)) return null;
  return normalizeTagName(raw);
}

export function parseCommitSubject(subject) {
  const text = String(subject ?? "").trim();
  if (!text) return null;
  if (PREPARE_RELEASE.test(text)) return { kind: "excluded", subject: text };

  const match = CONVENTIONAL.exec(text);
  if (!match) {
    return {
      kind: "other",
      type: "other",
      scope: undefined,
      breaking: false,
      description: text,
      subject: text,
    };
  }

  const type = match[1].toLowerCase();
  if (EXCLUDED_TYPES.has(type)) {
    return { kind: "excluded", type, subject: text };
  }

  return {
    kind: SECTION_TYPES.has(type) ? "section" : "other",
    type: SECTION_TYPES.has(type) ? type : "other",
    scope: match[2] || undefined,
    breaking: Boolean(match[3]),
    description: match[4].trim(),
    subject: text,
  };
}

export function formatChangeLine(commit) {
  const breaking = commit.breaking ? " **BREAKING**" : "";
  if (commit.kind === "other" && commit.type === "other" && commit.description === commit.subject) {
    return `- ${commit.subject}${breaking}`;
  }
  if (commit.scope) {
    return `- ${commit.scope}: ${commit.description}${breaking}`;
  }
  return `- ${commit.description}${breaking}`;
}

export function buildReleaseNotes({ tag, previousTag = null, subjects = [] }) {
  const current = normalizeTagName(tag, "release tag");
  const previous = previousTag ? normalizeTagName(previousTag, "previous tag") : null;
  const commits = subjects.map(parseCommitSubject).filter((item) => item && item.kind !== "excluded");

  const buckets = new Map(SECTIONS.map(([type]) => [type, []]));
  buckets.set("other", []);
  for (const commit of commits) {
    buckets.get(commit.type)?.push(commit);
  }

  const lines = [`## Changes since ${previous ?? "the beginning"}`, ""];
  let wroteSection = false;
  for (const [type, title] of SECTIONS) {
    const items = buckets.get(type) ?? [];
    if (items.length === 0) continue;
    wroteSection = true;
    lines.push(`### ${title}`, "");
    for (const item of items) lines.push(formatChangeLine(item));
    lines.push("");
  }
  const other = buckets.get("other") ?? [];
  if (other.length > 0) {
    wroteSection = true;
    lines.push("### Other", "");
    for (const item of other) lines.push(formatChangeLine(item));
    lines.push("");
  }
  if (!wroteSection) {
    lines.push("_No user-facing commits in this range._", "");
  }

  lines.push("---", "", PLATFORM_RELEASE_NOTES, "");
  const heading = [`# Open Console Gateway ${current}`, ""];
  if (current.includes("-")) {
    return [PRERELEASE_WARNING, "", ...heading, ...lines].join("\n");
  }
  return [...heading, ...lines].join("\n");
}

export function selectPreviousTag(currentTag, tags) {
  const current = normalizeTagName(currentTag, "release tag");
  const ordered = [];
  const seen = new Set();
  for (const tag of tags) {
    const normalized = normalizeTagNameLoose(tag);
    if (!normalized || seen.has(normalized)) continue;
    seen.add(normalized);
    ordered.push(normalized);
  }
  // Prefer version sort when git sort is unavailable (unit tests pass plain arrays).
  ordered.sort((left, right) => compareVersionTags(right, left));
  const index = ordered.indexOf(current);
  if (index === -1) {
    throw new Error(`Release tag ${current} was not found among repository tags.`);
  }
  const earlier = ordered.slice(index + 1);
  if (!current.includes("-")) {
    return earlier.find((tag) => !tag.includes("-")) ?? null;
  }
  return earlier[0] ?? null;
}

function compareVersionTags(left, right) {
  const parse = (tag) => {
    const match = VERSION_TAG.exec(tag);
    return {
      major: Number(match[1]),
      minor: Number(match[2]),
      patch: Number(match[3]),
      pre: tag.includes("-") ? tag.slice(tag.indexOf("-") + 1) : null,
    };
  };
  const a = parse(left);
  const b = parse(right);
  for (const field of ["major", "minor", "patch"]) {
    if (a[field] !== b[field]) return a[field] - b[field];
  }
  if (a.pre === b.pre) return 0;
  if (a.pre === null) return 1;
  if (b.pre === null) return -1;
  const aIdentifiers = a.pre.split(".");
  const bIdentifiers = b.pre.split(".");
  for (let index = 0; index < Math.max(aIdentifiers.length, bIdentifiers.length); index += 1) {
    const aIdentifier = aIdentifiers[index];
    const bIdentifier = bIdentifiers[index];
    if (aIdentifier === undefined) return -1;
    if (bIdentifier === undefined) return 1;
    if (aIdentifier === bIdentifier) continue;
    const aNumeric = /^\d+$/.test(aIdentifier);
    const bNumeric = /^\d+$/.test(bIdentifier);
    if (aNumeric && bNumeric) {
      return BigInt(aIdentifier) < BigInt(bIdentifier) ? -1 : 1;
    }
    if (aNumeric !== bNumeric) return aNumeric ? -1 : 1;
    return aIdentifier < bIdentifier ? -1 : 1;
  }
  return 0;
}

export function listVersionTags(runGit) {
  const output = runGit(["tag", "--list", "v*", "--sort=-v:refname"]);
  return output
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map(normalizeTagNameLoose)
    .filter(Boolean);
}

export function listCommitSubjects(runGit, { from, to }) {
  const range = from ? `${from}..${to}` : to;
  const output = runGit(["log", range, "--pretty=format:%s", "--no-merges"]);
  return output
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
}

function defaultRunGit(repoRoot, args) {
  const result = spawnSync("git", args, {
    cwd: repoRoot,
    encoding: "utf8",
    windowsHide: true,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(result.stderr?.trim() || `git ${args.join(" ")} failed with status ${result.status}`);
  }
  return result.stdout ?? "";
}

export function generateReleaseNotes({
  tag,
  previousTag,
  repoRoot = process.cwd(),
  runGit = (args) => defaultRunGit(repoRoot, args),
} = {}) {
  const current = normalizeTagName(tag, "release tag");
  const authoredNotes = join(repoRoot, "docs", "releases", `${current}.md`);
  if (!current.includes("-") && existsSync(authoredNotes)) {
    const notes = readFileSync(authoredNotes, "utf8").trim();
    if (!notes.startsWith(`# ${current}\n`) && !notes.startsWith(`# Open Console Gateway ${current}\n`)) {
      throw new Error(`Release notes heading does not match ${current}: ${authoredNotes}`);
    }
    return notes;
  }
  let previous;
  if (previousTag !== undefined) {
    previous = previousTag ? normalizeTagName(previousTag, "previous tag") : null;
  } else {
    const tags = listVersionTags(runGit);
    if (!tags.includes(current)) {
      throw new Error(`Release tag ${current} was not found among repository tags.`);
    }
    previous = selectPreviousTag(current, tags);
  }
  const subjects = listCommitSubjects(runGit, { from: previous, to: current });
  return buildReleaseNotes({ tag: current, previousTag: previous, subjects });
}

function parseOptions(args) {
  const options = {};
  for (let index = 0; index < args.length; index += 1) {
    const flag = args[index];
    if (!flag.startsWith("--")) {
      throw new Error(`Unexpected argument: ${flag}`);
    }
    const key = flag.slice(2);
    const value = args[index + 1];
    if (value === undefined || value.startsWith("--")) {
      options[key] = true;
      continue;
    }
    options[key] = value;
    index += 1;
  }
  return options;
}

function main(argv) {
  const options = parseOptions(argv);
  if (!options.tag) {
    throw new Error("Usage: generate-release-notes.mjs --tag vX.Y.Z [--previous vA.B.C] [--repo-root path]");
  }
  const notes = generateReleaseNotes({
    tag: options.tag,
    previousTag: Object.hasOwn(options, "previous") ? (options.previous || null) : undefined,
    repoRoot: options["repo-root"] ? resolve(options["repo-root"]) : process.cwd(),
  });
  process.stdout.write(notes.endsWith("\n") ? notes : `${notes}\n`);
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : undefined;
if (invokedPath === fileURLToPath(import.meta.url)) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(error.stack ?? error);
    process.exitCode = 1;
  }
}
