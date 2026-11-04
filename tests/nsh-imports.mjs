// The SDK-1 import gate (tools/check-nsh-imports.mjs) refuses what the Bot's
// loader refuses and passes what it passes (buwiz-bot
// app/src/mini_v1/load.rs:22 and `check_import`).
import assert from "node:assert/strict";
import test from "node:test";
import { check } from "../tools/check-nsh-imports.mjs";

for (const [name, source] of [
  ["SDK-1 values and types", `import { signal, computed, effect, untracked, resource, cleanup, Text, View, Scroll, Input, Overlay } from "nsh";\nimport type { JSX, Signal, Accessor, YNode } from "nsh";`],
  ["type-only names outside the table", `import type { InputRef, ScrollRef, ViewRef } from "nsh";\nimport { signal, type InputRef as R } from "nsh";`],
  ["nsh/base whole", `import { Input, Combobox, Radio, RadioGroup } from "nsh/base";`],
  ["buwiz", `import { buwiz, BuwizError } from "buwiz";`],
  ["multi-line import", `import {\n  signal,\n  computed,\n} from "nsh";`],
  ["a commented-out import", `// import { keymap } from "nsh";\n/* import { keymap } from "nsh"; */\nimport { signal } from "nsh";`],
  ["a string that names an import", `const s = 'import { keymap } from "nsh"';`],
]) {
  test(`accept ${name}`, () => assert.deepEqual(check(source), []));
}

for (const [name, source, problem] of [
  ["keymap from nsh", `import { signal, keymap, Text } from "nsh";`, "line 1: keymap from nsh"],
  ["a value renamed", `import { keymap as km } from "nsh";`, "line 1: keymap from nsh"],
  ["a multi-line import", `import {\n  signal,\n  keymap,\n} from "nsh";`, "line 1: keymap from nsh"],
  ["nsh's default", `import nsh from "nsh";`, "line 1: default from nsh"],
  ["nsh's namespace", `import * as nsh from "nsh";`, "line 1: nsh's namespace"],
  ["another nsh module", `import { press } from "nsh/test";`, "line 1: nsh/test"],
  ["a relative module", `import { x } from "./helpers";`, "line 1: ./helpers"],
  ["another buwiz name", `import { secret } from "buwiz";`, "line 1: secret from buwiz"],
  ["a dynamic import", `const m = await import("nsh");`, "import()"],
]) {
  test(`refuse ${name}`, () => assert.deepEqual(check(source), [problem]));
}

test("a problem names its line", () => {
  assert.deepEqual(check(`// one\n\n/* two\n three */\nimport { signal } from "nsh";\nimport { keymap } from "nsh";`), ["line 6: keymap from nsh"]);
});
