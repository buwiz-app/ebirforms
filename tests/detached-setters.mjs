import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const source = readFileSync(new URL("../tools/check-all.sh", import.meta.url), "utf8");
const definition = source.match(/^no_detached_setters\(\) \{\n.*?^\}/ms)?.[0];
assert.ok(definition, "the setter gate must define no_detached_setters");

function scan(handler) {
  const dir = mkdtempSync(join(tmpdir(), "ebirforms-setters-"));
  try {
    if (handler !== undefined) {
      mkdirSync(join(dir, "apps/nested"), { recursive: true });
      writeFileSync(join(dir, "apps/nested/probe.tsx"), `<Input ${handler} />\n`);
    }
    const result = spawnSync("bash", ["-c", `set -uo pipefail\n${definition}\nno_detached_setters`], {
      cwd: dir, encoding: "utf8", timeout: 5000,
    });
    assert.ifError(result.error);
    return result.status;
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

for (const [name, handler] of [
  ["single-letter identifier", "onChange={x.set}"],
  ["multi-letter identifier", "onChange={field.set}"],
  ["dotted identifier", "onChange={form.field.set}"],
  ["dollar identifier", "onChange={$field.set}"],
  ["spaced braces", "onChange={ field.set }"],
  ["spaced assignment", "onChange = {field.set}"],
  ["tabbed braces", "onChange=\t{\tfield.set\t}"],
  ["mixed whitespace", "onChange \t= { form.field.set\t }"],
]) {
  test(`reject ${name}`, () => assert.equal(scan(handler), 1));
}

for (const [name, handler] of [
  ["wrapped setter", "onChange={(v) => field.set(v)}"],
  ["spaced wrapped setter", "onChange = { (v) => field.set(v) }"],
  ["different method", "onChange={field.get}"],
  ["different method suffix", "onChange={field.setValue}"],
]) {
  test(`accept ${name}`, () => assert.equal(scan(handler), 0));
}

test("fail closed when apps is missing", () => assert.equal(scan(undefined), 2));
