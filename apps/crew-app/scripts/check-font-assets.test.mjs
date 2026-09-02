import { describe, expect, it } from "vitest";
import { EXPECTED_FACE_STEMS, checkFontAssets } from "./check-font-assets.mjs";

/** 통과하는 dist/assets 상태를 만든다. */
function goodOutput(overrides = {}) {
  const fonts = EXPECTED_FACE_STEMS.map((stem, i) => ({
    name: `${stem}-HASH${i}.woff2`,
    size: 1000 + i,
  }));
  const assetFiles = [
    ...fonts,
    { name: "index-CSSHASH.css", size: 43000 },
    { name: "index-JSHASH.js", size: 423000 },
  ];
  const cssText = fonts.map((f) => `src:url(/assets/${f.name}) format("woff2");`).join("\n");
  return { assetFiles, cssText, ...overrides };
}

describe("checkFontAssets", () => {
  it("정상: woff2 9개 / woff 0개 / 전부 CSS 에서 참조되면 통과한다", () => {
    const { ok, errors } = checkFontAssets(goodOutput());
    expect(errors).toEqual([]);
    expect(ok).toBe(true);
  });

  it("에러: .woff 가 방출되면 실패하고 파일명을 알려준다", () => {
    const out = goodOutput();
    out.assetFiles.push({ name: "ibm-plex-sans-latin-400-normal-XX.woff", size: 30000 });
    const { ok, errors } = checkFontAssets(out);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes(".woff 가 1개 방출됐다"))).toBe(true);
  });

  it("에러: 페이스 하나가 빠지면 개수와 이름 양쪽으로 잡는다", () => {
    const out = goodOutput();
    out.assetFiles = out.assetFiles.filter(
      (f) => !f.name.startsWith("ibm-plex-sans-kr-korean-400-normal-"),
    );
    const { ok, errors } = checkFontAssets(out);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes(".woff2 가 8개 방출됐다"))).toBe(true);
    expect(errors.some((e) => e.includes("'ibm-plex-sans-kr-korean-400-normal' 의 woff2 가"))).toBe(
      true,
    );
  });

  it("에러: 개수는 9개인데 구성이 바뀌면(중복 + 누락) 잡는다", () => {
    const out = goodOutput();
    const idx = out.assetFiles.findIndex((f) =>
      f.name.startsWith("ibm-plex-sans-kr-korean-700-normal-"),
    );
    out.assetFiles[idx] = { name: "ibm-plex-sans-kr-korean-400-normal-DUP.woff2", size: 500 };
    out.cssText += "\nsrc:url(/assets/ibm-plex-sans-kr-korean-400-normal-DUP.woff2);";
    const { ok, errors } = checkFontAssets(out);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes("'ibm-plex-sans-kr-korean-700-normal' 의 woff2 가 dist/assets 에 없다"))).toBe(
      true,
    );
    expect(errors.some((e) => e.includes("'ibm-plex-sans-kr-korean-400-normal' 의 woff2 가 2개다"))).toBe(
      true,
    );
  });

  it("에러: 해석되지 않은 @fontsource bare specifier 가 CSS 에 남아 있으면 잡는다", () => {
    const out = goodOutput();
    out.cssText += '\nsrc:url("@fontsource/ibm-plex-sans/files/x.woff2") format("woff2");';
    const { ok, errors } = checkFontAssets(out);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes("@fontsource/"))).toBe(true);
  });

  it("에러: 애셋은 방출됐는데 CSS 가 참조하지 않으면 잡는다", () => {
    const out = goodOutput({ cssText: "body{color:red}" });
    const { ok, errors } = checkFontAssets(out);
    expect(ok).toBe(false);
    expect(errors.filter((e) => e.includes("빌드된 CSS 어디에서도 참조되지 않는다"))).toHaveLength(
      EXPECTED_FACE_STEMS.length,
    );
  });

  it("경계값: 0바이트 폰트 파일을 잡는다", () => {
    const out = goodOutput();
    out.assetFiles[0].size = 0;
    const { ok, errors } = checkFontAssets(out);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes("0바이트다"))).toBe(true);
  });

  it("경계값: dist/assets 가 비어 있으면 즉시 실패한다", () => {
    const { ok, errors } = checkFontAssets({ assetFiles: [], cssText: "body{}" });
    expect(ok).toBe(false);
    expect(errors).toHaveLength(1);
    expect(errors[0]).toContain("파일이 하나도 없다");
  });

  it("경계값: CSS 가 비어 있으면(빈 문자열/공백) 즉시 실패한다", () => {
    for (const cssText of ["", "   \n  "]) {
      const { ok, errors } = checkFontAssets(goodOutput({ cssText }));
      expect(ok).toBe(false);
      expect(errors).toHaveLength(1);
      expect(errors[0]).toContain("빌드된 CSS 가 비어 있다");
    }
  });
});
