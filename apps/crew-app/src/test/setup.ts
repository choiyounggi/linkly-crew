import "@testing-library/jest-dom/vitest";

// jsdom (via this vitest/Node combo) does not wire up a working
// `localStorage` global — `typeof localStorage` is `undefined` even inside
// jsdom's own `window`. Minimal in-memory Storage-shaped polyfill, only
// installed if the real one isn't functional, so App.tsx's onboarding-flag
// persistence (plan D7) is actually testable. No new dependency.
function localStorageWorks(): boolean {
  try {
    const probe = (globalThis as { localStorage?: Storage }).localStorage;
    probe?.setItem("__probe__", "1");
    const ok = probe?.getItem("__probe__") === "1";
    probe?.removeItem("__probe__");
    return ok ?? false;
  } catch {
    return false;
  }
}

if (!localStorageWorks()) {
  class MemoryStorage implements Storage {
    private store = new Map<string, string>();
    get length(): number {
      return this.store.size;
    }
    clear(): void {
      this.store.clear();
    }
    getItem(key: string): string | null {
      return this.store.has(key) ? this.store.get(key)! : null;
    }
    key(index: number): string | null {
      return [...this.store.keys()][index] ?? null;
    }
    removeItem(key: string): void {
      this.store.delete(key);
    }
    setItem(key: string, value: string): void {
      this.store.set(key, value);
    }
  }
  const memoryStorage = new MemoryStorage();
  Object.defineProperty(globalThis, "localStorage", { value: memoryStorage, configurable: true });
  if (typeof window !== "undefined") {
    Object.defineProperty(window, "localStorage", { value: memoryStorage, configurable: true });
  }
}

// jsdom does not implement `matchMedia`. xterm.js's CoreBrowserService calls
// it unconditionally on `Terminal.open()` (t5-terminal's TerminalPanel,
// consumed by t8-fe-onboarding's OnboardingFlow/SettingsMenu — both now
// mount a real TerminalPanel, reachable from App.test.tsx's full-App
// renders). Minimal stub, only installed if missing, so `term.open()`
// doesn't throw under jsdom. No new dependency.
if (typeof window !== "undefined" && typeof window.matchMedia !== "function") {
  window.matchMedia = (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  }) as MediaQueryList;
}
