import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const styles = readFileSync(join(process.cwd(), "src", "App.css"), "utf8");

describe("session list responsive layout", () => {
  it("uses adjustable grid tracks in wide and stacked session layouts", () => {
    expect(styles).toMatch(/\.session-columns\s*\{[^}]*display:\s*grid;[^}]*grid-template-columns:[^}]*--session-primary-size/s);
    expect(styles).toMatch(/\.session-columns--stacked\s*\{[^}]*grid-template-rows:[^}]*--session-primary-size/s);
    expect(styles).not.toMatch(/\.column-resizer\s*\{[^}]*display:\s*none;/s);
  });

  it("uses an accessible, forgiving main splitter and keeps the reader visible", () => {
    expect(styles).toMatch(/\.session-layout\s*\{[^}]*container-type:\s*inline-size;/s);
    expect(styles).toMatch(/\.split-handle--vertical\s*\{[^}]*width:\s*12px;[^}]*flex:\s*0 0 12px;/s);
    expect(styles).toMatch(/\.split-handle--horizontal\s*\{[^}]*height:\s*12px;[^}]*flex:\s*0 0 12px;/s);
    expect(styles).toMatch(/\.transcript-panel\s*\{[^}]*min-width:\s*min\(420px,\s*52cqi\);/s);
    expect(styles).not.toMatch(/\.session-sidebar\s*\{[^}]*width:\s*45%\s*!important;/s);
  });

  it("gives terminal panes adjustable wide and stacked tracks", () => {
    expect(styles).toMatch(/\.terminal-grid\s*\{[^}]*grid-template-columns:[^}]*--terminal-primary-size/s);
    expect(styles).toMatch(/\.terminal-grid--stacked\s*\{[^}]*grid-template-rows:[^}]*--terminal-primary-size/s);
  });
});
