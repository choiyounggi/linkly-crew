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

/**
 * Returns every `border-radius:` declaration value that is not composed
 * exclusively of `var(--radius-*)` tokens (plus the literals `0`/`inherit`,
 * which name no scale step and so carry no drift risk).
 */
function findLiteralRadii(css) {
  const hits = [];
  for (const m of css.matchAll(/border-radius:\s*([^;]+);/g)) {
    const value = m[1].trim();
    const stripped = value.replace(/var\(--radius-[a-zA-Z0-9-]+\)/g, "").trim();
    if (stripped !== "" && stripped !== "0" && stripped !== "inherit") {
      hits.push(value);
    }
  }
  return hits;
}

/** Returns every non-OKLCH color literal (`#hex`, `rgb(`, `rgba(`, `hsl(`) found in css text. */
function findNonOklchColors(css) {
  const hits = [];
  for (const m of css.matchAll(/#[0-9a-fA-F]{3,8}\b|rgba?\(|hsla?\(/g)) {
    hits.push(m[0]);
  }
  return hits;
}

/**
 * CSS specificity per the cascade spec, as (id, class-or-pseudo-class, type)
 * counts, computed for a single compound selector (no combinators besides
 * whitespace-separated descendant chains; no :not()/:is() argument parsing —
 * not needed by any selector in this codebase). Ignores the universal `*`.
 */
function specificity(compoundSelector) {
  let ids = 0;
  let classes = 0;
  let types = 0;
  for (const part of compoundSelector.trim().split(/\s+/)) {
    classes += (part.match(/\.[a-zA-Z0-9_-]+/g) ?? []).length;
    classes += (part.match(/:(?!:)[a-zA-Z-]+/g) ?? []).length;
    ids += (part.match(/#[a-zA-Z0-9_-]+/g) ?? []).length;
    if (/^[a-zA-Z][a-zA-Z0-9]*/.test(part)) types += 1;
  }
  return [ids, classes, types];
}

/** True iff `a` strictly out-specifies `b` in cascade precedence (higher wins regardless of source order). */
function outSpecifies(a, b) {
  if (a[0] !== b[0]) return a[0] > b[0];
  if (a[1] !== b[1]) return a[1] > b[1];
  return a[2] > b[2];
}

/**
 * Splits CSS text into { selector, body } rules. Comments are stripped first
 * so a rule's selector capture can't accidentally swallow a preceding
 * comment block (a `[^{]+` selector match with no `}` boundary would span
 * into whatever text precedes it, including unrelated prior rules/comments —
 * this is exactly what made an earlier version of the none-cell specificity
 * regression test below pass even against the pre-fix, buggy CSS). Assumes
 * no nested braces (true of this codebase's CSS, per `getRuleBlock` above).
 */
function parseRules(text) {
  const withoutComments = text.replace(/\/\*[\s\S]*?\*\//g, "");
  const rules = [];
  for (const m of withoutComments.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    rules.push({ selector: m[1].trim(), body: m[2] });
  }
  return rules;
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

describe("findLiteralRadii (detector fixtures)", () => {
  it("flags a literal border-radius value", () => {
    expect(findLiteralRadii(".x {\n  border-radius: 4px;\n}\n")).toEqual(["4px"]);
  });

  it("does not flag a border-radius composed of --radius-* tokens", () => {
    expect(findLiteralRadii(".x {\n  border-radius: var(--radius-md);\n}\n")).toEqual([]);
  });
});

describe("findNonOklchColors (detector fixtures)", () => {
  it("flags a hex color", () => {
    expect(findNonOklchColors(".x { color: #fff; }")).toEqual(["#fff"]);
  });

  it("does not flag an oklch() color", () => {
    expect(findNonOklchColors(".x { color: oklch(50% 0 0); }")).toEqual([]);
  });
});

describe("specificity (detector fixtures)", () => {
  it("counts one type and one class for a type+class compound selector", () => {
    expect(specificity(".req-matrix td")).toEqual([0, 1, 1]);
  });

  it("counts one type and two classes for a type+class+class compound selector", () => {
    expect(specificity(".req-matrix td.req-matrix__cell--none")).toEqual([0, 2, 1]);
  });

  it("counts an id selector", () => {
    expect(specificity("#root")).toEqual([1, 0, 0]);
  });

  it("outSpecifies compares by id, then class, then type, in that order", () => {
    expect(outSpecifies([0, 2, 1], [0, 1, 1])).toBe(true);
    expect(outSpecifies([0, 1, 1], [0, 1, 1])).toBe(false);
    expect(outSpecifies([0, 1, 0], [1, 0, 0])).toBe(false);
  });
});

describe("parseRules (detector fixtures)", () => {
  it("isolates a rule's selector without swallowing a preceding comment block", () => {
    const text = [
      ".a {",
      "  color: red;",
      "}",
      "",
      "/* explains why .b looks the way it does",
      " * across several lines of prose */",
      ".b {",
      "  color: blue;",
      "}",
    ].join("\n");
    const rules = parseRules(text);
    expect(rules).toEqual([
      { selector: ".a", body: "\n  color: red;\n" },
      { selector: ".b", body: "\n  color: blue;\n" },
    ]);
  });

  it("isolates a rule's selector without swallowing the immediately preceding rule (no `}` boundary bug)", () => {
    const text = ".a {\n  color: red;\n}\n.b {\n  color: blue;\n}\n";
    const rules = parseRules(text);
    expect(rules.find((r) => r.selector === ".b").body).toBe("\n  color: blue;\n");
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

  // artifacts/board/search/rail's rows and the req-matrix specificity
  // regression test below them were removed alongside those views (plan
  // D5) — feature removal, not test weakening. features/thread was replaced
  // by features/chat (plan D7); this case now points at chat's equivalent
  // bold-party-name rule instead of being dropped.
  it.each([["features/chat/chat.css", ".message-row__from"]])(
    "%s %s resolves font-weight to var(--font-weight-bold), not a different token",
    (relPath, selector) => {
      const file = cssFiles.find((f) => f.path === relPath);
      expect(file, `expected to find ${relPath} under src/`).toBeTruthy();
      const block = getRuleBlock(file.text, selector);
      expect(block, `expected to find rule block for ${selector} in ${relPath}`).toBeTruthy();
      expect(block).toMatch(/font-weight:\s*var\(--font-weight-bold\);/);
    },
  );

  it("declares every border-radius through a --radius token", () => {
    const hits = cssFiles.flatMap(({ path: filePath, text }) =>
      findLiteralRadii(text).map((value) => `${filePath}: ${value}`),
    );
    expect(hits).toEqual([]);
  });

  it("tokens.css colors are OKLCH only", () => {
    const tokensFile = cssFiles.find((f) => f.path === "styles/tokens.css");
    expect(tokensFile, "expected to find styles/tokens.css under src/").toBeTruthy();
    expect(findNonOklchColors(tokensFile.text)).toEqual([]);
  });

  it.each([
    ["--control-h-md"],
    ["--focus-ring"],
    ["--color-primary"],
    ["--border-strong"],
    ["--scrim"],
  ])("%s is defined and referenced", (varName) => {
    expect(definedVars.has(varName), `expected ${varName} to be defined somewhere under src/`).toBe(true);
    expect(usedVars.has(varName), `expected ${varName} to be referenced (var(${varName})) somewhere under src/`).toBe(true);
  });

  // --radius-2xl (plan design.md D1) is mapped to the onboarding card, which
  // is features/** and owned by t3 — t2 only declares the token; the
  // reference lands when t3 skins onboarding on top of it.
  it("--radius-2xl is defined", () => {
    expect(definedVars.has("--radius-2xl"), "expected --radius-2xl to be defined somewhere under src/").toBe(true);
  });
});
