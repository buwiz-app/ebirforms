#!/usr/bin/env node
// tools/check-nsh-imports.mjs — the Bot's import table for an SDK-1 app
// (protocol 1.0, spec 6.3), checked before a release: the Bot refuses to
// open an app whose source imports anything else ("BIR Tax Profile uses
// keymap from nsh, which this Buwiz Bot version does not support").
//
// Mirrors buwiz-bot app/src/mini_v1/load.rs:22 (`SDK1_NSH`, `BUWIZ_EXPORTS`)
// and `check_import` there: `nsh/base` whole; from `nsh` only the SDK-1
// values below (type-only names are free); from `buwiz` its default and
// `buwiz`, `BuwizError`; no other module, no namespace import (`* as`).
// Keep the lists in step with load.rs.
//
// Usage: node tools/check-nsh-imports.mjs [files or dirs…] (default: apps/)
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

export const SDK1_NSH = [
  "signal", "computed", "effect", "untracked", "resource", "cleanup", "mount", "Text", "View", "Scroll", "Input",
  "Overlay", "JSX", "Signal", "Accessor", "YNode",
];
export const BUWIZ_EXPORTS = ["buwiz", "BuwizError"];

/** The source without comments, strings kept (a specifier is one). */
function stripComments(source) {
  let out = "";
  for (let i = 0; i < source.length; ) {
    const c = source[i];
    const n = source[i + 1];
    if (c === "/" && n === "/") {
      while (i < source.length && source[i] !== "\n") i++;
    } else if (c === "/" && n === "*") {
      const end = source.indexOf("*/", i + 2);
      const cut = end < 0 ? source.length : end + 2;
      out += source.slice(i, cut).replace(/[^\n]/g, " ");
      i = cut;
    } else if (c === '"' || c === "'" || c === "`") {
      let j = i + 1;
      while (j < source.length && source[j] !== c) j += source[j] === "\\" ? 2 : 1;
      out += source.slice(i, j + 1);
      i = j + 1;
    } else {
      out += c;
      i++;
    }
  }
  return out;
}

/** Each `import … from "x"`, `import "x"` and `export … from "x"`:
 *  { line, spec, clause, exported }. */
export function imports(source) {
  const text = stripComments(source);
  const found = [];
  const re = /(^|[;\n}])\s*(import|export)\s*([^;'"`]*?)\s*(?:from\s*)?(["'])([^"']+)\4/g;
  for (let m; (m = re.exec(text)); ) {
    const [, , kind, clause] = m;
    // `export const x = "…"` is no import: a re-export names its module after `from`
    if (kind === "export" && !/\bfrom\s*$/.test(m[0].slice(0, m[0].lastIndexOf(m[4])))) continue;
    const line = text.slice(0, m.index + m[0].indexOf(kind, m[1].length)).split("\n").length;
    found.push({ line, spec: m[5], clause: clause.replace(/\bfrom\s*$/, "").trim(), exported: kind === "export" });
  }
  if (/\bimport\s*\(/.test(text)) found.push({ line: 0, spec: "import()", clause: "", exported: false });
  return found;
}

/** The names a clause binds: { name, typeOnly }, or null for `* as x`. */
function names(clause) {
  if (/\*/.test(clause)) return null;
  let rest = clause.trim();
  let allTypes = false;
  if (/^type\s/.test(rest)) {
    allTypes = true;
    rest = rest.slice(5).trim();
  }
  const out = [];
  const braced = rest.match(/\{([^}]*)\}/);
  const head = rest.replace(/\{[^}]*\}/, "").replace(/,/g, " ").trim();
  if (head) out.push({ name: "default", typeOnly: allTypes });
  for (const part of braced ? braced[1].split(",") : []) {
    const words = part.trim().split(/\s+/).filter(Boolean);
    if (!words.length) continue;
    const typeOnly = allTypes || (words[0] === "type" && words.length > 1 && words[1] !== "as");
    out.push({ name: typeOnly && words[0] === "type" ? words[1] : words[0], typeOnly });
  }
  return out;
}

/** What `source` imports outside the table: one message per problem. */
export function check(source) {
  const problems = [];
  for (const imp of imports(source)) {
    const at = imp.line ? `line ${imp.line}: ` : "";
    if (imp.spec === "nsh/base") continue;
    if (imp.spec === "nsh" || (imp.spec === "buwiz" && !imp.exported)) {
      const list = imp.spec === "nsh" ? SDK1_NSH : BUWIZ_EXPORTS;
      const bound = names(imp.clause);
      if (!bound) {
        problems.push(`${at}${imp.spec}'s namespace`);
        continue;
      }
      for (const { name, typeOnly } of bound) {
        if (typeOnly || (imp.spec === "buwiz" && name === "default")) continue;
        if (!list.includes(name)) problems.push(`${at}${name} from ${imp.spec}`);
      }
      continue;
    }
    problems.push(`${at}${imp.spec}`);
  }
  return problems;
}

function files(path) {
  if (!statSync(path).isDirectory()) return [path];
  return readdirSync(path).flatMap((name) => (name.startsWith(".") ? [] : files(join(path, name))));
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const repo = fileURLToPath(new URL("..", import.meta.url));
  const roots = process.argv.length > 2 ? process.argv.slice(2) : [join(repo, "apps")];
  const sources = roots.flatMap(files).filter((f) => f.endsWith(".tsx") || f.endsWith(".ts"));
  let bad = 0;
  for (const file of sources) {
    for (const problem of check(readFileSync(file, "utf8"))) {
      console.error(`${relative(repo, file)}: ${problem} is outside the Bot's SDK-1 import table (buwiz-bot app/src/mini_v1/load.rs:22)`);
      bad++;
    }
  }
  if (!sources.length) {
    console.error("no app sources found");
    process.exit(2);
  }
  console.log(bad ? `${bad} import(s) the Bot refuses` : `nsh imports: pass (${sources.length} apps)`);
  process.exit(bad ? 1 : 0);
}
