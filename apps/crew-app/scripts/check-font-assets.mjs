/**
 * 빌드 산출물 폰트 가드.
 *
 * 이 앱의 폰트는 src/index.css 의 손으로 쓴 @font-face 9개가
 * `url("@fontsource/.../files/*.woff2")` 라는 bare specifier 를 참조하고,
 * Vite 가 그것을 해석해 dist/assets 에 해시 애셋으로 방출하는 구조다.
 * 이 해석이 Vite / @fontsource 업그레이드로 바뀌면 폰트가 번들에서 조용히
 * 사라진다 — vitest 는 jsdom 이라 폰트를 로드하지 않으므로 감지하지 못한다.
 * 그래서 검사를 빌드 산출물 위에 둔다 (`npm run build` 의 마지막 스텝).
 */
import { readdir, readFile, stat } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

/** dist/assets 에 있어야 하는 9개 페이스의 파일명 stem (뒤에 -<hash>.woff2 가 붙는다). */
export const EXPECTED_FACE_STEMS = [
  "ibm-plex-mono-latin-300-normal",
  "ibm-plex-mono-latin-400-normal",
  "ibm-plex-mono-latin-700-normal",
  "ibm-plex-sans-latin-300-normal",
  "ibm-plex-sans-latin-400-normal",
  "ibm-plex-sans-latin-700-normal",
  "ibm-plex-sans-kr-korean-300-normal",
  "ibm-plex-sans-kr-korean-400-normal",
  "ibm-plex-sans-kr-korean-700-normal",
];

/**
 * 순수 판정 함수 — 파일시스템에 접근하지 않는다.
 *
 * @param {object} input
 * @param {Array<{name: string, size: number}>} input.assetFiles dist/assets 의 전체 파일 목록
 * @param {string} input.cssText 빌드된 CSS 전체를 이어붙인 텍스트
 * @returns {{ok: boolean, errors: string[]}}
 */
export function checkFontAssets({ assetFiles, cssText }) {
  const errors = [];

  if (!Array.isArray(assetFiles) || assetFiles.length === 0) {
    errors.push("dist/assets 에 파일이 하나도 없다 — 빌드가 산출물을 만들지 못했다.");
    return { ok: false, errors };
  }
  if (typeof cssText !== "string" || cssText.trim() === "") {
    errors.push("빌드된 CSS 가 비어 있다 — dist/assets 에서 .css 를 찾지 못했다.");
    return { ok: false, errors };
  }

  const woff = assetFiles.filter((f) => f.name.endsWith(".woff"));
  const woff2 = assetFiles.filter((f) => f.name.endsWith(".woff2"));

  // 1. .woff 는 0개여야 한다. woff2 만 참조하도록 @font-face 를 손으로 썼기 때문.
  if (woff.length !== 0) {
    errors.push(
      `.woff 가 ${woff.length}개 방출됐다 (0개여야 함): ${woff.map((f) => f.name).join(", ")}. ` +
        "src/index.css 의 @font-face 가 woff2 외의 format 을 참조하고 있는지 확인할 것.",
    );
  }

  // 2. .woff2 는 정확히 9개여야 한다.
  if (woff2.length !== EXPECTED_FACE_STEMS.length) {
    errors.push(
      `.woff2 가 ${woff2.length}개 방출됐다 (${EXPECTED_FACE_STEMS.length}개여야 함). ` +
        "Vite 가 @fontsource bare specifier 를 해석하지 못했을 수 있다.",
    );
  }

  // 3. 9개 페이스가 각각 정확히 하나씩 있어야 한다.
  //    개수만 맞고 구성이 바뀌는 경우(예: korean-400 이 빠지고 다른 게 둘)를 잡는다.
  const matched = new Set();
  for (const stem of EXPECTED_FACE_STEMS) {
    const hits = woff2.filter((f) => f.name.startsWith(`${stem}-`));
    if (hits.length === 0) {
      errors.push(`페이스 '${stem}' 의 woff2 가 dist/assets 에 없다.`);
    } else if (hits.length > 1) {
      errors.push(
        `페이스 '${stem}' 의 woff2 가 ${hits.length}개다: ${hits.map((f) => f.name).join(", ")}.`,
      );
    }
    for (const hit of hits) matched.add(hit.name);
  }
  for (const f of woff2) {
    if (!matched.has(f.name)) {
      errors.push(`예상 목록에 없는 woff2 가 방출됐다: ${f.name}.`);
    }
  }

  // 4. 빈 파일이 아니어야 한다.
  for (const f of woff2) {
    if (f.size <= 0) {
      errors.push(`폰트 파일이 0바이트다: ${f.name}.`);
    }
  }

  // 5. 방출된 woff2 는 전부 CSS 에서 참조돼야 한다.
  //    애셋은 남았는데 CSS 가 그것을 가리키지 않으면 화면에는 폰트가 없다.
  for (const f of woff2) {
    if (!cssText.includes(f.name)) {
      errors.push(`woff2 '${f.name}' 가 방출됐지만 빌드된 CSS 어디에서도 참조되지 않는다.`);
    }
  }

  // 6. 해석되지 않은 bare specifier 가 CSS 에 남아 있으면 안 된다.
  //    이것이 정확히 "Vite 가 url() 을 해석하지 못했다" 의 신호다.
  if (cssText.includes("@fontsource/")) {
    errors.push(
      "빌드된 CSS 에 해석되지 않은 '@fontsource/' bare specifier 가 남아 있다 — " +
        "Vite 가 url() 안의 패키지 경로를 애셋으로 리라이트하지 못했다.",
    );
  }

  return { ok: errors.length === 0, errors };
}

/** dist/assets 를 읽어 checkFontAssets 에 넘길 입력을 만든다. */
async function readBuildOutput(assetsDir) {
  let names;
  try {
    names = await readdir(assetsDir);
  } catch {
    return null;
  }
  const assetFiles = [];
  for (const name of names) {
    const s = await stat(join(assetsDir, name));
    if (s.isFile()) assetFiles.push({ name, size: s.size });
  }
  const cssParts = await Promise.all(
    assetFiles
      .filter((f) => f.name.endsWith(".css"))
      .map((f) => readFile(join(assetsDir, f.name), "utf8")),
  );
  return { assetFiles, cssText: cssParts.join("\n") };
}

async function main() {
  const assetsDir = join(process.cwd(), "dist", "assets");
  const output = await readBuildOutput(assetsDir);

  if (output === null) {
    console.error(`[font-guard] FAIL — ${assetsDir} 를 읽을 수 없다. 빌드가 실행됐는지 확인할 것.`);
    process.exit(1);
  }

  const { ok, errors } = checkFontAssets(output);
  if (!ok) {
    console.error("[font-guard] FAIL — 빌드 산출물의 폰트 구성이 예상과 다르다:");
    for (const e of errors) console.error(`  - ${e}`);
    process.exit(1);
  }

  const total = output.assetFiles
    .filter((f) => f.name.endsWith(".woff2"))
    .reduce((sum, f) => sum + f.size, 0);
  console.log(
    `[font-guard] OK — woff2 ${EXPECTED_FACE_STEMS.length}개 / woff 0개, 폰트 총합 ${total} bytes.`,
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}
