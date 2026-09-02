import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const SRC_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function findCssFiles(dir) {
  const out = [];
  for (const entry of readdirSync(dir)) {
    const full = path.join(dir, entry);
    if (statSync(full).isDirectory()) {
      out.push(...findCssFiles(full));
    } else if (entry.endsWith(".css")) {
      out.push(full);
    }
  }
  return out;
}

/**
 * Pure analysis over a list of { path, text } CSS files — no filesystem access,
 * so detector behavior can be proven against synthetic fixtures independently
 * of the real src/ tree.
 */
function analyzeCssFiles(files) {
  const filesBySelector = new Map();
  const definedVars = new Set();
  const usedVars = new Set();

  for (const { path: filePath, text } of files) {
    for (const m of text.matchAll(/^([.#][a-zA-Z0-9_ .:-]*)\{/gm)) {
      const selector = m[1].trim();
      const seen = filesBySelector.get(selector) ?? new Set();
      seen.add(filePath);
      filesBySelector.set(selector, seen);
    }

    for (const m of text.matchAll(/^\s*(--[a-zA-Z0-9-]+)\s*:/gm)) {
      definedVars.add(m[1]);
    }

    for (const m of text.matchAll(/var\((--[a-zA-Z0-9-]+)/g)) {
      usedVars.add(m[1]);
    }
  }

  const duplicateSelectors = [...filesBySelector.entries()]
    .filter(([, seen]) => seen.size > 1)
    .map(([selector, seen]) => `${selector} -> ${[...seen].join(", ")}`);

  const undefinedVars = [...usedVars].filter((v) => !definedVars.has(v)).sort();

  return { duplicateSelectors, undefinedVars, definedVars, usedVars };
}

/** Extracts the `{ ... }` body of the first `selector { ... }` block in text (no nested braces in this codebase's CSS). */
function getRuleBlock(text, selector) {
  const start = text.indexOf(`${selector} {`);
  if (start === -1) return null;
  const bodyStart = text.indexOf("{", start) + 1;
  const bodyEnd = text.indexOf("}", bodyStart);
  return text.slice(bodyStart, bodyEnd);
}

describe("analyzeCssFiles (detector fixtures)", () => {
  it("flags a selector declared in two different files", () => {
    const { duplicateSelectors } = analyzeCssFiles([
      { path: "a.css", text: ".search-box {\n  position: relative;\n}\n" },
      { path: "b.css", text: ".search-box {\n  flex-shrink: 0;\n}\n" },
    ]);
    expect(duplicateSelectors).toEqual([".search-box -> a.css, b.css"]);
  });

  it("does not flag the same selector repeated within one file, or distinct selectors across files", () => {
    const { duplicateSelectors } = analyzeCssFiles([
      { path: "a.css", text: ".card {\n  color: red;\n}\n.card {\n  color: blue;\n}\n" },
      { path: "b.css", text: ".other {\n  color: green;\n}\n" },
    ]);
    expect(duplicateSelectors).toEqual([]);
  });

  it("flags a var(--x) reference whose custom property is never defined", () => {
    const { undefinedVars } = analyzeCssFiles([
      { path: "a.css", text: ":root {\n  --known: 1;\n}\n.x {\n  color: var(--known);\n  width: var(--typo-unknown);\n}\n" },
    ]);
    expect(undefinedVars).toEqual(["--typo-unknown"]);
  });

  it("does not flag a var(--x) reference whose property is defined in a different file", () => {
    const { undefinedVars } = analyzeCssFiles([
      { path: "tokens.css", text: ":root {\n  --known: 1;\n}\n" },
      { path: "a.css", text: ".x {\n  color: var(--known);\n}\n" },
    ]);
    expect(undefinedVars).toEqual([]);
  });
});

describe("CSS integrity (apps/crew-app/src)", () => {
  const cssFiles = findCssFiles(SRC_DIR).map((filePath) => ({
    path: path.relative(SRC_DIR, filePath),
    text: readFileSync(filePath, "utf8"),
  }));
  const { duplicateSelectors, undefinedVars, definedVars, usedVars } = analyzeCssFiles(cssFiles);

  it("declares no selector in more than one CSS file", () => {
    expect(duplicateSelectors).toEqual([]);
  });

  it("references no var(--x) whose custom property is never defined", () => {
    expect(undefinedVars).toEqual([]);
  });

  it("does not declare or reference --font-weight-semibold (no 600 font face is bundled; resolved to var(--font-weight-bold))", () => {
    expect(definedVars.has("--font-weight-semibold")).toBe(false);
    expect(usedVars.has("--font-weight-semibold")).toBe(false);
  });

  it.each([
    ["features/artifacts/artifacts.css", ".artifact-detail__name"],
    ["features/board/board.css", ".board-card__badge"],
    ["features/search/search.css", ".search-box__result-kind"],
    ["features/rail/rail.css", ".rail-card__name"],
    ["features/rail/rail.css", ".rail-badge"],
    ["features/thread/thread.css", ".thread-row__parties"],
  ])("%s %s resolves font-weight to var(--font-weight-bold), not a different token", (relPath, selector) => {
    const file = cssFiles.find((f) => f.path === relPath);
    expect(file, `expected to find ${relPath} under src/`).toBeTruthy();
    const block = getRuleBlock(file.text, selector);
    expect(block, `expected to find rule block for ${selector} in ${relPath}`).toBeTruthy();
    expect(block).toMatch(/font-weight:\s*var\(--font-weight-bold\);/);
  });
});
