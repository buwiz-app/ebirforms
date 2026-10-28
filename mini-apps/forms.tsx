// BIR Forms mini app: the bir-shell form page (app/shell/form_page.js) and
// the ways into it (main.js / home.js / taxpayer.js / library.js), as one
// Native Shell module. Opened full screen by Rox for ONE taxpayer branch:
// `rox.props.ticket`, a ticket BIR issued for the branch or one of its
// returns. The app asks BIR what the ticket stands for (`bir_ticket_subject`)
// and every host call goes through Rox's bridge to BIR's named tool for that
// function (`bir_app_<fn>`), held to the ticket. Rox's `/taxform` opens it on
// one return (`{ticket, form, period, action, draft_id?}`): `view`
// read-only, `file` to fill, `submit` to validate and confirm with the
// user's own Submit. The schema-driven renderer covers all 43 forms the way
// form_page.js does: parts -> items -> fields, schedules, locks, profile
// boxes, penalties, samples, amendments, validation, save and queue.

import { signal, computed, untracked, Text, View, Scroll, Input } from "nsh";
import type { YNode } from "nsh";
import { rox } from "rox";

/** An `<Input>`'s text from its `onChange`: `{ value }` since nsh's events
 *  became objects (#558), the text itself before. */
const typed = (e: { value: string } | string): string => (typeof e === "string" ? e : e.value);

// =====================================================================
// HOST CALLS: every call to the BIR daemon (and Rox) lives in this section.
// Names, argument order and result shapes follow the DECLARATIONS in
// crates/bir-shell/src/{bir_module,screen_form_module,tax_profile_module,
// dashboard_module}.rs.
// =====================================================================

/**
 * Set by Rox before the module runs (`rox.props`). `ticket` names the branch or one of its returns;
 * `form` + `period` (`2026-Q3`, `2026-01`, `2025`, `2026-10-26`) + `action` (+ `draft_id`) open one return
 * (Rox's `/taxform`); `form_code`/`year`/`period`/`key` deep-link to a return too.
 */
type RoxProps = {
  ticket?: string;
  form?: string; action?: "view" | "file" | "submit"; draft_id?: number | string | null;
  form_code?: string; year?: number | string; period?: number | string; key?: number | string | null;
};
const roxProps = (): RoxProps => (rox.props || {}) as RoxProps;
const digits = (v: unknown) => String(v ?? "").replace(/\D/g, "");

/** What BIR says the props' ticket stands for (`bir_ticket_subject`). */
type Subject = { kind: "taxpayer" | "branch" | "return"; tin: string; form?: string; period?: string; title: string };
const subject = signal<Subject | null>(null);
/** The branch's own ticket: what every call is held to and what tools take as `tin`. */
let BRANCH = "";

/** One BIR host function through its named tool (`bir_app_<fn>`), held to the branch's ticket. */
async function bir<T = any>(fn: string, ...args: unknown[]): Promise<T> {
  // a signed call carries whole numbers only: the arguments (a draft's amounts) go as JSON text
  const r: any = await rox.call(`bir_app_${fn}`, BRANCH ? { args_json: JSON.stringify(args), ticket: BRANCH } : { args_json: JSON.stringify(args) });
  return r?.result as T;
}

/** One of the BIR server's own tools: the payments. `tin` there is the branch's ticket. */
async function tool<T = any>(name: string, args: Record<string, unknown>): Promise<T> {
  const r: any = await rox.call(name, args);
  return r?.result as T;
}

/** base64 of bytes, 3 at a time (the bridge carries JSON only). */
function base64(bytes: Uint8Array): string {
  const A = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let out = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const n = (bytes[i] << 16) | ((bytes[i + 1] ?? 0) << 8) | (bytes[i + 2] ?? 0);
    out += A[(n >> 18) & 63] + A[(n >> 12) & 63] + (i + 1 < bytes.length ? A[(n >> 6) & 63] : "=") + (i + 2 < bytes.length ? A[n & 63] : "=");
  }
  return out;
}

/** A receipt's extensions, as the manifest's `pick_file` grants them. */
const RECEIPTS = [".pdf", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".heic"];
/** Bytes per upload call: base64 of this stays under the bridge's 256 KiB argument cap. */
const CHUNK = 180 * 1024;

/**
 * Rox's file picker: the receipt the user chose, sent to BIR in chunks (`bir_app_upload`).
 * `null` when cancelled. The app never sees a path.
 */
async function pickReceipt(): Promise<{ upload: string; name: string } | null> {
  const files = await rox.pickFile({ extensions: RECEIPTS, multiple: false });
  const file = files[0];
  if (!file) return null;
  const bytes = await file.read();
  let upload = "";
  let at = 0;
  do {
    const r: any = await rox.call("bir_app_upload", { ...(upload ? { upload } : {}), data: base64(bytes.subarray(at, at + CHUNK)) });
    upload = String(r?.result?.upload || "");
    at += CHUNK;
  } while (at < bytes.length);
  return { upload, name: file.name };
}

/** Attaches a return to the chat Rox opened this app from (the assistant reads it there). */
const attachReturn = (s: FS) => rox.attach("taxform", `${BRANCH}/${s.code}-${tokenPeriod(s)}`);

type Edits = Record<string, string>;
const H = {
  // catalogs and the taxpayer
  index: () => bir<IndexEntry[]>("screen_forms_index"),
  schema: (code: string) => bir<Schema>("screen_form_schema", code),
  taxpayers: () => bir<any[]>("list_taxpayers"),
  taxYears: (tin: string) => bir<number[]>("list_tax_years", tin),
  yearlyForms: (tin: string, year: number) => bir<string[]>("yearly_forms", tin, year),
  templates: (tin: string) => bir<any[]>("list_form_templates", tin),
  deadlines: (from: string, to: string) => bir<any[]>("deadlines_between", from, to),
  asNeeded: (year: number) => bir<any[]>("as_needed_forms", year),
  returns: (which: "filed" | "saved" | "all") => bir<any[]>("list_returns", which),
  // opening a return
  open: (tin: string, code: string, year: number, period: number, key: number | null, newKey: number | null) =>
    bir<any>("screen_form_open", tin, code, year, period, key, newKey),
  sample: (tin: string, code: string) => bir<any>("screen_form_sample", tin, code),
  samplePeriod: (tin: string, code: string, parts: Edits) => bir<any>("screen_form_sample_period", tin, code, parts),
  amend: (tin: string, code: string, year: number, period: number, key: number | null) => bir<any>("screen_form_amend", tin, code, year, period, key),
  filedOriginal: (code: string, id: number) => bir<any>("screen_form_filed_original", code, id),
  // the page loop
  fill: (code: string, draft: any, edits: Edits, unlocked: string[]) => bir<any>("screen_form_fill", code, draft, edits, unlocked),
  useProfile: (code: string, year: number, period: number, draft: any) => bir<any>("screen_form_use_profile", code, year, period, draft),
  validate: (code: string, draft: any) => bir<any>("screen_form_validate", code, draft),
  save: (code: string, draft: any) => bir<any>("screen_form_save", code, draft),
  checkNotice: (code: string, draft: any, token: string) => bir<null>("screen_form_check_notice", code, draft, token),
  notesSeen: (keys: string[]) => bir<string[]>("screen_form_notes_seen", keys),
  // queue
  queue: (code: string, draft: any) => bir<number>("screen_form_queue", code, draft),
  cancelQueue: (code: string, draft: any) => bir<null>("screen_form_cancel_queue", code, draft),
  queuePeriod: (tin: string, code: string, year: number, quarter: number | null) => bir<number>("queue_form_draft", tin, code, year, quarter),
  cancelQueued: (tin: string, code: string, year: number, quarter: number | null) => bir<null>("cancel_queued", tin, code, year, quarter),
  // the assistant's standing OK to submit a form for this taxpayer
  grants: (tin: string) => bir<{ tin: string; form: string; granted_at: string }[]>("list_send_grants", tin),
  grant: (tin: string, code: string) => bir<any>("grant_send", tin, code),
  revoke: (tin: string, code: string) => bir<null>("revoke_send", tin, code),
  // payments and the printable filing record (the server's own tools)
  payments: (form: string, period: string) => tool<{ payments: Payment[] }>("bir_payments_list", { tin: BRANCH, form, period }),
  recordPayment: (form: string, period: string, d: { paid_at: string; amount: string; channel: string; reference: string }) =>
    tool<{ payment: Payment }>("bir_record_payment", {
      tin: BRANCH, form, period, channel: d.channel, amount: d.amount, paid_at: d.paid_at,
      ...(d.reference.trim() ? { reference: d.reference.trim() } : {}),
    }),
  removePayment: (form: string, period: string, id: string) => tool<{ removed: boolean }>("bir_card_remove_payment", { tin: BRANCH, form, period, id }),
  markPaid: (form: string, period: string, proof?: { upload: string; name: string }) =>
    tool<{ payment: Payment }>("bir_mark_paid", proof ? { tin: BRANCH, form, period, proof_upload: proof.upload, proof_name: proof.name } : { tin: BRANCH, form, period }),
};

// =====================================================================
// Types
// =====================================================================

type Field = { k: string; l: string; t: string; c?: any; r?: any; m?: number; w?: boolean; o?: any[]; pool?: { keys: string[] }; grp?: string; rg?: number };
type Col = { l: string; t: string; c?: any; w?: boolean; o?: any[] };
type Item = {
  no?: string; l: string; p?: number; f?: Field[];
  rows?: number; cols?: Col[]; keys?: string[][]; start?: number; row_labels?: { caption: string; rows: string[] }; numbered?: boolean; calendar?: boolean;
  one_date?: boolean; from?: { k: string; to: string; l: string }; note?: string; typed_tin?: boolean;
};
type Part = { n: string; items: Item[] };
type Schema = { c: string; t: string; rev: string; freq: string; pages: number; n: number; q: number; parts: Part[] };
type IndexEntry = { c: string; t: string; rev: string; freq: string; pages: number; n: number; q: number };
type Opt = { value: string; label: string; selected?: boolean };
type DateSpec = { single?: Field; month?: Field; day?: Field | null; year?: Field; oneKey?: string };
type Pen = { keys: string[]; edited: boolean; kept?: boolean; applies: boolean };
type Err = { field: string; message: string; keys?: string[]; dismiss?: string; dismiss_label?: string };
type PeriodPart = { key: string; part: string; quarter: number | null };
/** A recorded payment (bir_payments_list). `channel` "manual" is "marked paid" with no details. */
type Payment = {
  id: string; channel: string; reference: string; amount: number | null; fee: number | null; total: number | null;
  paid_at: string | null; recorded_at: string; source: "email" | "receipt" | "manual"; proof: { kind: string; name?: string | null };
  message_id?: string | null;
};

interface Opts {
  tin: string; code: string; year: number; period: number;
  returnKey?: number | null; sample?: boolean; amend?: boolean; filedOriginalId?: number; back?: Opts;
  /** Rox's `view`: every box read-only. */
  viewOnly?: boolean;
  /** Rox's `submit`: validate on open, then ask before the user's own Submit. */
  submitAsk?: boolean;
}

interface Idx {
  keyIndex: Map<string, string>; kind: Map<string, string>; computed: Set<string>; whole: Set<string>;
  pools: Map<string, { keys: string[] }>; oneDates: Map<string, DateSpec>;
}

interface Picker { id: string; date: DateSpec; y: number; m: number; sel: string; list: "month" | "year" | null }

interface FS extends Opts {
  schema: Schema | null; idx: Idx | null; loading: boolean; error: string | null;
  draft: any; status: string; editable: boolean;
  values: Record<string, string>; options: Record<string, any[]>; locks: Record<string, string>; lockNotes: Record<string, string>;
  notesAlways: Set<string>; profile: Set<string>; profileEdit: Set<string>; editing: Set<string>; opened: Set<string>;
  focusKey: string | null; profileOpen: Set<number>;
  blank: Set<string>; needs: Record<string, string>; whys: Record<string, string>; waits: Record<string, string>; off: Record<string, string>;
  penalties: Pen[]; penaltyOpen: Set<string>; poolTexts: Record<string, string>;
  profileUpdates: string[]; profileDiffers: string[]; notes: { key: string; text: string; recorded: boolean }[];
  saved: boolean; quiet: { field: string; keys: string[] } | null;
  typed: Set<string>; typedEdits: Edits;
  errors: Err[]; messages: Record<string, string>; checkable: Record<string, { token: string; label: string }[]>; formErrors: string[];
  checking: boolean; openParts: Set<number>; pending: Edits; dirty: boolean;
  openSelect: string | null; moreOpen: Set<string>; dateErrors: Record<string, string>; datesEdited: Set<string>;
  text: Record<string, string>;
  periodParts: Map<string, PeriodPart>; periodSet: boolean; periodValues: Edits; periodAsked: string | null; periodAnswered: string | null;
  periodComplete: boolean; periodInFlight: string | null; periodSeq: number; periodToken: number; opens: any; existing: any;
  saveAfterAnswer: { then: (() => void) | null } | null;
  submittedAt: string | null; confirmedAt: string | null; amends: string | null; filedOriginal: any; filedOriginalView: any;
  removal: { r: any; edits: Edits; typed: Edits; draft: any } | null; chosenFrom: Edits;
  editSeq: number; validateSeq: number; chain: Promise<unknown>;
  picker: Picker | null; printResult: { sample: boolean; error: string } | null;
  /** Whether the assistant may submit this form for this taxpayer (null: not read yet). */
  assistantMay: boolean | null;
  /** This return's recorded payments (null: not read yet, or not a stored return). */
  payments: Payment[] | null;
  /** Why the payments could not be read (signed out, server down), in plain words. */
  paymentsError?: string | null;
  /** The Mark as paid details, while the user types them. */
  payForm?: { paid_at: string; amount: string; channel: string; reference: string } | null;
  /** The payment whose Remove asks "Remove this payment?" */
  removeAsk?: string | null;
  paying: boolean;
}

// =====================================================================
// Pure helpers (ported from form_page.js, store.js, periods.js)
// =====================================================================

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_LONG = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

/** "1601-C" and "1601c" name 1601C. */
const normCode = (code: unknown) => String(code ?? "").replace(/[-\s]/g, "").toUpperCase();

function errText(e: unknown): string {
  const m = e && typeof e === "object" && "message" in e ? (e as any).message : e;
  return String(m).replace(/^Error:\s*/, "").replace(/^`[^`]+`:\s*/, "");
}

function optionOf(o: any): Opt | null {
  if (typeof o !== "string") return o;
  if (/^"/.test(o)) return null;
  const m = /^(\S+) - (.+)$/.exec(o);
  return m ? { value: m[1], label: o } : { value: o, label: o };
}

function isTinItem(it: Item): boolean {
  const fs = it.f || [];
  const named = /\bTIN\b|Identification/i.test(it.l) || fs.some((x) => /\bTIN\d?\b|txtTIN/i.test(`${x.l} ${x.k}`));
  return named && fs.length >= 3 && fs.length <= 5 && fs.every((x) => x.t === "text" || x.t === "int");
}

function withoutHidden(schema: Schema): Schema {
  const parts = schema.parts
    .map((p) => ({
      ...p,
      items: p.items
        .filter((it) => !it.rows || (it.cols || []).some((c) => c.t !== "hidden"))
        .map((it) => (it.f ? { ...it, f: it.f.filter((f) => f.t !== "hidden") } : it))
        .filter((it) => it.rows || (it.f && it.f.length) || it.note),
    }))
    .filter((p) => p.items.length);
  return { ...schema, parts };
}

function partName(p: Part, i: number) {
  return i === 0 && /^Page \d+$/.test(p.n) ? "Return details" : p.n;
}

function parseTypedDate(text: unknown): { y: number; m: number; d: number } | null {
  const v = String(text || "").trim();
  if (!v) return null;
  const MON = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
  let y: any, m: any, d: any, hit: RegExpExecArray | null;
  if ((hit = /^(\d{2})(\d{2})(\d{4})$/.exec(v))) [, m, d, y] = hit;
  else if ((hit = /^(\d{4})-(\d{1,2})-(\d{1,2})$/.exec(v))) [, y, m, d] = hit;
  else if ((hit = /^(\d{1,2})[/-](\d{1,2})[/-](\d{4})$/.exec(v))) [, m, d, y] = hit;
  else if ((hit = /^([a-z]{3})[a-z]*\.?\s+(\d{1,2}),?\s+(\d{4})$/i.exec(v))) {
    m = MON.indexOf(hit[1].toLowerCase()) + 1;
    d = hit[2];
    y = hit[3];
  } else return null;
  [y, m, d] = [Number(y), Number(m), Number(d)];
  if (y < 1900 || y > 2100 || m < 1 || m > 12 || d < 1) return null;
  if (d > new Date(Date.UTC(y, m, 0)).getUTCDate()) return null;
  return { y, m, d };
}
const formatMDY = ({ y, m, d }: { y: number; m: number; d: number }) => `${String(m).padStart(2, "0")}/${String(d).padStart(2, "0")}/${y}`;
const formatLongDate = ({ y, m, d }: { y: number; m: number; d: number }) => `${MONTHS[m - 1]} ${d}, ${y}`;
function shownDate(text: unknown) {
  const date = parseTypedDate(text);
  return date ? formatLongDate(date) : String(text ?? "");
}
const DATE_ERROR = "Enter a valid date as MM/DD/YYYY (e.g. 12/01/2020 or 12012020)";
const SAMPLE_NOTE =
  "A sample from the Form library. Save makes it your return for its period, with what you typed; if the period is not set yet, Save asks for it, and if that period already has a return, you choose what to do with it. Pick a form under Forms to start a blank return instead.";
const PERIOD_PART_LABELS: Record<string, string> = {
  year: "Year (YYYY)", month: "Month (MM)", year_end_month: "Year-end month (MM)", day: "Day (DD)", date: "Date (MM/DD/YYYY)", quarter: "Quarter",
};
const FILED_KEPT = "The filed version is kept in this return's history.";
const OFF_OPACITY = 0.45;

function needsText(tag: string) {
  const text = String(tag || "").trim();
  return text.length > 90 ? `${text.slice(0, 89).trimEnd()}…` : text;
}

function itemKeys(it: Item): string[] {
  if (it.rows) return (it.keys || []).flat();
  return (it.f || []).flatMap((f) => (f.pool ? f.pool.keys : [f.k]));
}

function formatMoney(text: unknown, whole = false): string {
  const n = Number(String(text).replace(/[₱,\s]/g, ""));
  if (String(text).trim() === "" || Number.isNaN(n)) return String(text);
  if (whole && !Number.isInteger(n)) return String(text);
  const [int, frac] = Math.abs(n).toFixed(whole ? 0 : 2).split(".");
  const grouped = `${n < 0 ? "-" : ""}${int.replace(/\B(?=(\d{3})+(?!\d))/g, ",")}`;
  return whole ? grouped : `${grouped}.${frac}`;
}

function wholePesosOf(text: unknown) {
  const n = Number(String(text ?? "").replace(/[₱,\s]/g, ""));
  if (String(text ?? "").trim() === "" || !Number.isFinite(n)) return null;
  const amount = Math.round(n);
  if (amount === 0) return null;
  return { amount, rounded: amount !== n };
}

const ATC_TITLES: Record<string, string> = {
  II011: "Compensation Income",
  II012: "Business Income – Graduated IT Rates",
  II013: "Mixed Income – Graduated IT Rates",
  II014: "Income from Profession – Graduated IT Rates",
  II015: "Business Income – 8% IT Rate",
  II016: "Mixed Income – 8% IT Rate",
  II017: "Income from Profession – 8% IT Rate",
};
function choiceLabel(f: Field) {
  const text = f.l.replace(/^.*?:\s*/, "");
  const code = /^A?TC\s+([A-Z]{2}\d{3})$/.exec(text)?.[1];
  return code ? (ATC_TITLES[code] ? `${code}  ${ATC_TITLES[code]}` : code) : text;
}

function dateParts(fs: Field[]): DateSpec | null {
  const find = (re: RegExp, not?: RegExp) =>
    fs.find((f) => f.t !== "radio" && f.t !== "chk" && re.test(`${f.l} ${String(f.k).split(":").pop()}`) && !(not && not.test(`${f.l} ${f.k}`)));
  const month = find(/month|\bmm\b/i, /months|no\.? of/i);
  const year = find(/year|yyyy|\byr\b/i, /month|years ended|no\.? of/i);
  const day = find(/\bday\b|\bdd\b|[a-z]day\b|day\d*$/i, /days|month|year/i);
  if (!month || !year || month === year) return null;
  return { month, day: day && day !== month && day !== year ? day : null, year };
}
function oneDate(it: Item): DateSpec | null {
  if (!it.one_date) return null;
  const date = dateParts(it.f || []);
  return date ? { ...date, oneKey: `${date.month!.k}#date` } : null;
}
function oneDateText(values: Record<string, string>, date: DateSpec) {
  const parts = [date.month, date.day, date.year].map((f) => String((f && values[f.k]) ?? "").trim());
  return parts.every((v) => !v) ? "" : parts.join("/");
}

function shortDate(text: unknown): string {
  if (!text) return "";
  const ts = /^(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2})/.exec(String(text));
  if (ts) {
    const local = new Date(Date.UTC(+ts[1], +ts[2] - 1, +ts[3], +ts[4], +ts[5]));
    return `${MONTHS[local.getMonth()]} ${local.getDate()}, ${local.getFullYear()}`;
  }
  const [y, m, d] = String(text).split(/[ T]/)[0].split("-").map(Number);
  if (!y || !m || !d) return String(text);
  return `${MONTHS[m - 1]} ${d}, ${y}`;
}

function formatTin(root: unknown, branch?: string) {
  const r = digits(root);
  const base = r.length >= 9 ? `${r.slice(0, 3)}-${r.slice(3, 6)}-${r.slice(6, 9)}` : r;
  return branch ? `${base}-${branch}` : base;
}

function isoDate(date: Date) {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}
function addDays(date: Date, days: number) {
  const next = new Date(date.getTime());
  next.setDate(next.getDate() + days);
  return next;
}
function daysUntil(iso: string) {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const [y, m, d] = iso.split("-").map(Number);
  return Math.round((new Date(y, m - 1, d).getTime() - today.getTime()) / 86400000);
}

function periodOptions(freq: string, year: number): { value: number; label: string }[] {
  const now = new Date();
  const lastMonth = year < now.getFullYear() ? 12 : year > now.getFullYear() ? 1 : now.getMonth() + 1;
  const out: { value: number; label: string }[] = [];
  if (freq === "Monthly") {
    for (let m = lastMonth; m >= 1; m--) out.push({ value: m, label: `${MONTHS_LONG[m - 1]} ${year}` });
    return out;
  }
  if (freq === "Quarterly") {
    for (let q = Math.ceil(lastMonth / 3); q >= 1; q--) out.push({ value: q, label: `Q${q} ${year}` });
    return out;
  }
  if (freq === "As needed") return [{ value: 1, label: `${year}, first return` }];
  return [{ value: 0, label: `TY ${year}` }];
}
function periodLabel(freq: string, year: number, period: number) {
  if (freq === "Monthly" && period >= 1 && period <= 12) return `${MONTHS_LONG[period - 1]} ${year}`;
  if (freq === "Quarterly" && period >= 1 && period <= 4) return `Q${period} ${year}`;
  if (freq === "As needed") return `${year} #${period || 1}`;
  return `TY ${year}`;
}

function existingOfferFor(existing: any) {
  const label = existing.label;
  const has = (a: string) => (existing.actions || []).includes(a);
  const keep = { id: "keep", label: "Keep this form", hint: "Stay in the sample; the period clears.", variant: "secondary" };
  if (existing.status === "Draft") {
    return {
      title: `You already have a ${label} draft`,
      subtitle: existing.updated_at ? `edited ${shortDate(existing.updated_at)}.` : "",
      choices: [{ id: "load", label: "Load saved draft", hint: "Continue where you left off.", variant: "primary" }, keep],
      fastLane: true,
    };
  }
  if (existing.status === "Queued") {
    return {
      title: `${label} is queued to send to BIR`, subtitle: "",
      choices: [{ id: "view", label: "View it", hint: "Read-only while it waits to send.", variant: "primary" }, keep],
      fastLane: false,
    };
  }
  const filed = existing.status === "Submitted" ? "submitted" : "filed";
  const choices = [{ id: "view", label: "View read-only", hint: `See exactly what was ${filed}.`, variant: "primary" }];
  if (has("amend")) choices.push({ id: "amend", label: "Amend this return", hint: `New amended return. ${FILED_KEPT}`, variant: "danger" });
  choices.push(keep);
  return { title: `${label} was ${filed}`, subtitle: "", choices, fastLane: false };
}

function controlWidth(t: string, m?: number) {
  if (t === "money") return 230;
  if (t === "pct") return 96;
  if (t === "date") return 150;
  if (t === "int") return m && m <= 4 ? 96 : 150;
  if (t === "sel") return 260;
  if (t === "text" && m && m <= 4) return 96;
  return 320;
}

function indexSchema(schema: Schema): Idx {
  const idx: Idx = { keyIndex: new Map(), kind: new Map(), computed: new Set(), whole: new Set(), pools: new Map(), oneDates: new Map() };
  schema.parts.forEach((p, pi) =>
    p.items.forEach((it, ii) => {
      for (const key of itemKeys(it)) idx.keyIndex.set(key, `${pi}:${ii}`);
      if (it.rows) {
        (it.cols || []).forEach((c, ci) =>
          (it.keys || []).forEach((row) => {
            idx.kind.set(row[ci], c.t);
            if (c.c) idx.computed.add(row[ci]);
            if (c.w) idx.whole.add(row[ci]);
          }),
        );
      } else {
        for (const f of it.f || []) {
          idx.kind.set(f.k, f.t);
          if (f.c) idx.computed.add(f.k);
          if (f.w) idx.whole.add(f.k);
          if (f.pool) idx.pools.set(f.k, f.pool);
        }
        const od = oneDate(it);
        if (od) {
          idx.oneDates.set(od.oneKey!, od);
          idx.keyIndex.set(od.oneKey!, `${pi}:${ii}`);
          idx.kind.set(od.oneKey!, "date");
        }
      }
    }),
  );
  return idx;
}

// =====================================================================
// App state (signals) and the theme
// =====================================================================

const C = {
  pane: "bg-white dark:bg-[#161616]",
  card: "bg-[#f5f5f5] dark:bg-[#1f1f1f]",
  inset: "bg-black/5 dark:bg-white/5",
  line: "border-black/10 dark:border-white/10",
  fg: "text-[#1c1c1c] dark:text-[#f5f5f5]",
  fg2: "text-[#1c1c1c99] dark:text-[#f5f5f599]",
  fg3: "text-[#1c1c1c66] dark:text-[#f5f5f566]",
  danger: "text-[#c62828] dark:text-[#ff6b6b]",
  accent: "text-[#1680f3] dark:text-[#1884fc]",
  accentBg: "bg-[#1680f3] dark:bg-[#1884fc]",
  dangerSoft: "bg-[#c628281a] dark:bg-[#ff6b6b26]",
  accentSoft: "bg-[#1680f31f] dark:bg-[#1884fc33]",
  input: "placeholder:text-[#8e8e93] h-8 px-2 rounded-md border border-black/10 dark:border-white/10 bg-white dark:bg-[#161616] text-sm text-[#1c1c1c] dark:text-[#f5f5f5]",
};

/** One tick for every in-place change of the form state: everything on screen reads it. */
const rev = signal(0);
let bumpQueued = false;
function bump() {
  if (bumpQueued) return;
  bumpQueued = true;
  queueMicrotask(() => {
    bumpQueued = false;
    rev.update((n) => n + 1);
  });
}

const cur = signal<FS | null>(null);
const view = signal<"picker" | "form">("picker");
/** The open form's token: a reply for an older opening is dropped. */
let T = 0;

type SamplePeriodAsk = { state: FS; then: (() => void) | null; values: Edits; quarter: string | null; error: string };
type Overlay = { kind: "existing"; existing: any } | { kind: "sample-period"; ask: SamplePeriodAsk } | { kind: "removal" } | null;
const overlay = signal<Overlay>(null);
/** "Don't ask again for drafts (this session)". */
let fastLaneDrafts = false;

type Toast = { id: number; text: string; tone: "info" | "danger" | "success"; undo?: () => void };
const toastSig = signal<Toast | null>(null);
let toastId = 0;
function toast(text: string, tone: Toast["tone"] = "info", undo?: () => void) {
  if (!String(text || "").trim()) return;
  const id = ++toastId;
  toastSig.set({ id, text, tone, undo });
  setTimeout(() => {
    if (toastSig()?.id === id) toastSig.set(null);
  }, undo ? 8000 : 5000);
}
const offerUndo = (what: string, undo: () => void) => toast(`${what}.`, "info", undo);

// The taxpayer branch this app is focused on (its 14-digit TIN, from BIR).
const TIN = () => digits(subject()?.tin);
const ROOT = () => TIN().slice(0, 9);

function openTaxProfile() {
  rox.open({ app: "tax-profile", ticket: BRANCH, props: { ticket: BRANCH } }).catch((e) => toast(errText(e), "danger"));
}

// =====================================================================
// Form page actions (FormPageActions in form_page.js)
// =====================================================================

function newState(opts: Opts): FS {
  return {
    ...opts,
    schema: null, idx: null, loading: true, error: null,
    draft: null, status: "Draft", editable: true,
    values: {}, options: {}, locks: {}, lockNotes: {},
    notesAlways: new Set(), profile: new Set(), profileEdit: new Set(), editing: new Set(), opened: new Set(),
    focusKey: null, profileOpen: new Set(),
    blank: new Set(), needs: {}, whys: {}, waits: {}, off: {},
    penalties: [], penaltyOpen: new Set(), poolTexts: {},
    profileUpdates: [], profileDiffers: [], notes: [],
    saved: false, quiet: null, typed: new Set(), typedEdits: {},
    errors: [], messages: {}, checkable: {}, formErrors: [],
    checking: false, openParts: new Set([0]), pending: {}, dirty: false,
    openSelect: null, moreOpen: new Set(), dateErrors: {}, datesEdited: new Set(), text: {},
    periodParts: new Map(), periodSet: false, periodValues: {}, periodAsked: null, periodAnswered: null,
    periodComplete: false, periodInFlight: null, periodSeq: 0, periodToken: 0, opens: null, existing: null, saveAfterAnswer: null,
    submittedAt: null, confirmedAt: null, amends: null, filedOriginal: null, filedOriginalView: null,
    removal: null, chosenFrom: {}, editSeq: 0, validateSeq: 0, chain: Promise.resolve(),
    picker: null, printResult: null, assistantMay: null, payments: null, paying: false,
  };
}

/** Runs `fn` after every earlier host round-trip of this form: each fill takes the draft the last one returned. */
function serial<R>(s: FS, fn: () => Promise<R>): Promise<R> {
  const run = s.chain.then(fn, fn);
  s.chain = run.catch(() => undefined);
  return run;
}
const live = (s: FS, token: number) => token === T && cur() === s;

const schemaCache = new Map<string, Schema>();

function openForm(opts: Opts, then: ((error: string | null) => void) | null = null) {
  closeOffer();
  const token = ++T;
  const s = newState(opts);
  cur.set(s);
  view.set("form");
  bump();
  (async () => {
    try {
      let schema = schemaCache.get(opts.code);
      if (!schema) {
        schema = withoutHidden(await H.schema(opts.code));
        schemaCache.set(opts.code, schema);
      }
      if (token !== T) return;
      s.schema = schema;
      s.idx = indexSchema(schema);
      bump();
    } catch (e) {
      if (token !== T) return;
      s.loading = false;
      s.error = errText(e);
      bump();
      then?.(s.error);
      return;
    }
    try {
      const key = opts.returnKey ?? null;
      const r = opts.sample
        ? await H.sample(opts.tin, opts.code)
        : opts.filedOriginalId != null
          ? await H.filedOriginal(opts.code, opts.filedOriginalId)
          : opts.amend
            ? await H.amend(opts.tin, opts.code, opts.year, opts.period, key)
            : await H.open(opts.tin, opts.code, opts.year, opts.period, key, null);
      if (token !== T) return;
      s.loading = false;
      takeOpened(s, r);
      bump();
      then?.(null);
      recordNotesWhenShown(s, token);
      readGrant(s, token);
      readPayments(s, token);
      if (s.submitAsk && s.editable && !s.sample) validate(s);
    } catch (e) {
      if (token !== T) return;
      s.loading = false;
      s.error = errText(e);
      bump();
      then?.(s.error);
    }
  })();
}

function takeOpened(s: FS, r: any) {
  s.draft = r.draft;
  s.status = r.status;
  s.editable = r.editable && !s.viewOnly;
  s.blank = new Set(r.blank || []);
  s.periodParts = new Map((r.period_parts || []).map((p: PeriodPart) => [p.key, p]));
  s.periodSet = Boolean(r.period_set);
  s.submittedAt = r.submitted_at || null;
  s.confirmedAt = r.confirmed_at || null;
  s.amends = r.amends || null;
  s.filedOriginal = r.filed_original || null;
  s.filedOriginalView = r.filed_original_view || null;
  s.values = shown(s, r.values || {});
  if (s.periodSet) {
    s.periodValues = Object.fromEntries([...s.periodParts].map(([k, p]) => [k, p.part === "quarter" ? String(s.values[k] === "true") : String(s.values[k] ?? "")]));
    s.periodAsked = JSON.stringify(s.periodValues);
    s.periodAnswered = s.periodAsked;
    s.periodComplete = true;
  }
  s.options = r.options || {};
  s.locks = r.locks || {};
  s.lockNotes = r.lock_notes || {};
  s.notesAlways = new Set(r.notes_always || []);
  s.profile = new Set(r.profile || []);
  s.profileEdit = new Set(r.profile_edit || []);
  s.needs = r.needs || {};
  s.whys = r.whys || {};
  s.waits = r.waits || {};
  s.off = r.off || {};
  s.penalties = r.penalties || [];
  s.poolTexts = r.pools || {};
  s.profileUpdates = r.profile_updates || [];
  s.profileDiffers = r.profile_differs || [];
  s.notes = (r.notes || []).map((n: any) => ({ key: n.key, text: n.text, recorded: false }));
  s.saved = Boolean(r.draft && r.draft.id);
  s.quiet = r.quiet || null;
  // Every box shows the draft as opened (InputStates are made fresh on open).
  s.text = {};
  setErrors(s, r.errors);
}

function unquiet(s: FS, keys?: string[]) {
  if (s.quiet && (!keys || keys.some((k) => s.quiet!.keys.includes(k)))) s.quiet = null;
}

/** The opening's one-time notes are recorded seen once the page has drawn them. */
function recordNotesWhenShown(s: FS, token: number) {
  if (!s.notes.some((n) => !n.recorded)) return;
  setTimeout(async () => {
    if (!live(s, token)) return;
    const pending = s.notes.filter((n) => !n.recorded);
    if (!pending.length) return;
    try {
      const recorded = await H.notesSeen(pending.map((n) => n.key));
      s.notes = s.notes.filter((n) => n.recorded || recorded.includes(n.key)).map((n) => ({ ...n, recorded: true }));
      bump();
    } catch (_) {
      // Left unrecorded: the next opening offers the notes again.
    }
  }, 500);
}

// ----- what a box shows

const kindOf = (s: FS, k: string) => s.idx?.kind.get(k) || "text";
const isWhole = (s: FS, k: string) => Boolean(s.idx?.whole.has(k));
const isComputed = (s: FS, k: string) => Boolean(s.idx?.computed.has(k));
const poolOf = (s: FS, k: string) => s.idx?.pools.get(k) || null;
const poolText = (s: FS, f: Field) => String(s.poolTexts[f.pool!.keys[0]] ?? "");
const itemIdForKey = (s: FS, k: string) => s.idx?.keyIndex.get(k);

/** What an untouched input box shows for the draft's value (InputState's first text). */
function display(s: FS, k: string): string {
  const od = s.idx?.oneDates.get(k);
  if (od) return shownDate(oneDateText(s.values, od));
  if (poolOf(s, k)) return String(s.poolTexts[k] ?? "");
  const v = String(s.values[k] ?? "");
  const kind = kindOf(s, k);
  if (kind === "money" && v !== "") return formatMoney(v, isWhole(s, k));
  if (kind === "date") return shownDate(v);
  return v;
}
const textOf = (s: FS, k: string) => (k in s.text ? s.text[k] : display(s, k));

/** Computed and locked boxes follow the draft; user boxes keep what was typed. */
function syncComputed(s: FS) {
  for (const k of Object.keys(s.text)) {
    if (!(isLocked(s, k) || isComputed(s, k))) continue;
    s.text[k] = display(s, k);
  }
}
/** A box the host also fills shows its new value; boxes in this batch keep the typing. */
function syncChanged(s: FS, before: Record<string, string>, edits: Edits) {
  for (const k of Object.keys(s.text)) {
    if (k in edits) continue;
    const od = s.idx?.oneDates.get(k);
    if (od) {
      const keys = [od.month!.k, od.day?.k, od.year!.k].filter(Boolean) as string[];
      if (keys.some((p) => p in edits)) continue;
      if (oneDateText(before, od) !== oneDateText(s.values, od)) s.text[k] = display(s, k);
      continue;
    }
    if (poolOf(s, k)) {
      s.text[k] = display(s, k);
      continue;
    }
    if ((before[k] ?? "") !== (s.values[k] ?? "")) s.text[k] = display(s, k);
  }
}

function shown(s: FS, values: Record<string, string>) {
  if (!s.sample || !s.blank.size) return values;
  const out = { ...values };
  for (const k of s.blank) {
    if (s.periodParts.has(k)) out[k] = s.periodValues[k] ?? "";
    else if (k in out && !s.typed.has(k)) out[k] = "";
  }
  return out;
}

const isOff = (s: FS, k: string) => s.off[k] || "";
const penaltyGroupOf = (s: FS, k: string) => s.penalties.find((g) => g.keys.includes(k)) || null;
function isLocked(s: FS, k: string) {
  if (isOff(s, k)) return true;
  const g = penaltyGroupOf(s, k);
  if (g) return !g.applies || !(g.edited || s.penaltyOpen.has(k));
  return s.locks[k] !== undefined && !(s.profileEdit.has(k) && s.editing.has(k));
}
const isComputedBox = (s: FS, k: string) => isComputed(s, k) || /^computed/.test(s.locks[k] || "");
function reasonFor(s: FS, k: string) {
  if (s.blank.has(k) && s.needs[k]) return s.needs[k];
  return s.whys[k] || s.waits[k] || "";
}
const unlockedFor = (s: FS, edits: Edits) => Object.keys(edits).filter((k) => s.locks[k] === undefined || s.opened.has(k) || s.penaltyOpen.has(k));
function accepted(s: FS, edits: Edits, rejected?: Record<string, string>) {
  for (const [k, v] of Object.entries(edits)) {
    if (rejected && k in rejected) continue;
    s.typed.add(k);
    if (s.sample) s.typedEdits[k] = v;
  }
}

function lockNotesFor(s: FS, it: Item): string[] {
  if (!s.editable || !s.draft) return [];
  const notes: string[] = [];
  const add = (text: string) => {
    if (text && !notes.includes(text)) notes.push(text);
  };
  const note = (k: string) => add(s.lockNotes[k] || (isComputed(s, k) ? "Computed by the form." : ""));
  const shut = (k: string, c: any) => Boolean(c) || isLocked(s, k);
  const waits = (k: string) => {
    if (isOff(s, k) && isComputedBox(s, k)) add("Computed by the form.");
    return Boolean(reasonFor(s, k) || isOff(s, k));
  };
  const always = (k: string) => s.notesAlways.has(k);
  const ticked = (k: string) => ["true", "1", "Y"].includes(String(s.values[k] ?? ""));
  const blankText = (k: string, t: string) => {
    const v = String(s.values[k] ?? "").trim();
    if (t === "money" && isComputedBox(s, k)) return false;
    if (t === "sel") return v === "" || /^(0+|-)$/.test(v);
    return v === "";
  };
  if (it.rows) {
    (it.keys || []).forEach((row) =>
      row.forEach((k, c) => {
        const col = it.cols![c];
        if (col.t === "hidden" || !shut(k, col.c) || waits(k)) return;
        if (always(k) || (col.t === "radio" || col.t === "chk" ? !ticked(k) : blankText(k, col.t))) note(k);
      }),
    );
    return notes;
  }
  const fs = it.f || [];
  const radios = fs.filter((f) => f.t === "radio");
  if (radios.length && radios.every((f) => shut(f.k, f.c)) && !radios.some((f) => ticked(f.k)) && !radios.some((f) => waits(f.k))) note(radios[0].k);
  for (const f of fs) {
    if (f.t === "radio" || f.t === "hidden" || !shut(f.k, f.c) || waits(f.k)) continue;
    if (always(f.k) || (f.t === "chk" ? !ticked(f.k) : blankText(f.k, f.t))) note(f.k);
  }
  return notes;
}

function tuckedItems(s: FS, pi: number): number[] {
  const part = s.schema?.parts[pi];
  if (!part || !s.profile.size) return [];
  const out: number[] = [];
  part.items.forEach((it, ii) => {
    if (it.rows || it.l === "Additional details") return;
    const typed = (it.f || []).filter((f) => !f.c);
    if (typed.length && typed.every((f) => s.profile.has(f.k))) out.push(ii);
  });
  return out;
}

/** Bind each validation message to an item: key, item number, TIN, label words, else the first item. */
function setErrors(s: FS, errors: Err[] | undefined) {
  s.errors = (errors || []).filter((e) => !(s.quiet && e.field === s.quiet.field));
  s.messages = {};
  s.checkable = {};
  if (!s.schema) return;
  const items: { id: string; it: Item }[] = [];
  s.schema.parts.forEach((p, pi) => p.items.forEach((it, ii) => items.push({ id: `${pi}:${ii}`, it })));
  const words = (t: string) => String(t).toLowerCase().match(/[a-z]{4,}/g) || [];
  for (const e of s.errors) {
    let id: string | null | undefined = null;
    for (const k of e.keys || []) {
      id = itemIdForKey(s, k);
      if (id) break;
    }
    if (!id) {
      const m = /\bItem\s+0*(\d+[A-Z]?)\b/i.exec(e.message);
      if (m) id = items.find((x) => String(x.it.no || "").replace(/^0+/, "").toUpperCase() === m[1].toUpperCase())?.id;
    }
    if (!id && /\bTIN\b/.test(e.message)) id = items.find((x) => isTinItem(x.it))?.id;
    if (!id) {
      const w = new Set(words(`${e.field} ${e.message}`));
      let best = 0;
      for (const x of items) {
        const n = words(x.it.l).filter((v) => w.has(v)).length;
        if (n > best) {
          best = n;
          id = x.id;
        }
      }
    }
    id = id || (items[0] && items[0].id);
    if (id) s.messages[id] = s.messages[id] ? `${s.messages[id]} · ${e.message}` : e.message;
    if (id && e.dismiss) (s.checkable[id] ||= []).push({ token: e.dismiss, label: e.dismiss_label || "Checked" });
  }
}

function setDateError(s: FS, k: string, msg: string) {
  const id = itemIdForKey(s, k);
  if (!id) return;
  if ((s.dateErrors[id] || "") === msg) return;
  if (msg) s.dateErrors[id] = msg;
  else delete s.dateErrors[id];
  bump();
}

// ----- edits

const VALIDATE_DELAY = 300;
const PERIOD_ASK_DELAY = 300;

/** Stage an edit; the draft takes it on the next flush (300 ms after the last edit). */
function edit(s: FS, k: string, value: string) {
  if (!s.editable) return;
  if (s.periodParts.has(k)) return setPeriodPart(s, { [k]: value });
  s.pending[k] = value;
  s.dirty = true;
  unquiet(s, [k]);
  const token = T;
  const seq = ++s.editSeq;
  setTimeout(() => {
    if (!live(s, token) || seq !== s.editSeq) return;
    flush(s);
  }, VALIDATE_DELAY);
}

/** Choice controls commit at once. */
function choose(s: FS, edits: Edits) {
  if (!s.editable) return;
  const period = Object.fromEntries(Object.entries(edits).filter(([k]) => s.periodParts.has(k)));
  if (Object.keys(period).length) {
    setPeriodPart(s, period);
    edits = Object.fromEntries(Object.entries(edits).filter(([k]) => !(k in period)));
    if (!Object.keys(edits).length) return;
  }
  Object.assign(s.pending, edits);
  s.dirty = true;
  unquiet(s, Object.keys(edits));
  s.chosenFrom = Object.fromEntries(Object.keys(edits).map((k) => [k, s.values[k]]));
  for (const [k, v] of Object.entries(edits)) s.values[k] = String(v);
  bump();
  flush(s);
}

function applyFill(s: FS, r: any, edits: Edits) {
  accepted(s, edits, r.rejected);
  if (s.sample && Array.isArray(r.blank)) s.blank = new Set(r.blank);
  const before = s.values;
  s.draft = r.draft;
  s.values = shown(s, r.values || {});
  s.options = r.options || {};
  if (Array.isArray(r.penalties)) s.penalties = r.penalties;
  if (r.pools) s.poolTexts = r.pools;
  if (Array.isArray(r.notes_always)) s.notesAlways = new Set(r.notes_always);
  for (const [k, lock] of Object.entries(r.relocks || {})) {
    if (lock == null) {
      delete s.locks[k];
      delete s.lockNotes[k];
    } else {
      s.locks[k] = String(lock);
      const note = r.relock_notes?.[k];
      if (note) s.lockNotes[k] = note;
    }
  }
  syncComputed(s);
  syncChanged(s, before, edits);
  for (const [k, msg] of Object.entries(r.rejected || {})) s.messages[itemIdForKey(s, k) || "form"] = String(msg);
}

/** Apply staged edits to the typed draft, then validate. */
function flush(s: FS): Promise<void> {
  if (!s.draft) return Promise.resolve();
  const token = T;
  return serial(s, async () => {
    if (!live(s, token)) return;
    const edits = s.pending;
    s.pending = {};
    if (Object.keys(edits).length) {
      retireRemovalUndo();
      try {
        const r = await H.fill(s.code, s.draft, edits, unlockedFor(s, edits));
        if (!live(s, token)) return;
        if (r.removes) return holdRemoval(s, r, edits);
        applyFill(s, r, edits);
      } catch (e) {
        s.pending = { ...edits, ...s.pending };
        toast(errText(e), "danger");
      }
    }
    validate(s);
    bump();
  });
}

function validate(s: FS) {
  if (!s.draft) return;
  const token = T;
  s.checking = true;
  const seq = ++s.validateSeq;
  const draft = s.draft;
  H.validate(s.code, draft)
    .then((r) => {
      if (!live(s, token) || seq !== s.validateSeq) return;
      if (s.draft === draft) s.draft = r.draft;
      if (s.sample && Array.isArray(r.blank)) s.blank = new Set(r.blank);
      s.values = shown(s, { ...s.values, ...(r.values || {}) });
      s.options = r.options || {};
      s.off = r.off || {};
      if (Array.isArray(r.penalties)) s.penalties = r.penalties;
      if (r.pools) s.poolTexts = r.pools;
      syncComputed(s);
      setErrors(s, r.errors);
      s.formErrors = [];
    })
    .catch((e) => {
      if (live(s, token)) s.formErrors = [`Could not check the return: ${errText(e)}`];
    })
    .finally(() => {
      if (token === T && seq === s.validateSeq) s.checking = false;
      bump();
    });
  bump();
}

// ----- an answer that removes typed rows asks first

let removalUndo: { state: FS; undo: () => void } | null = null;

function holdRemoval(s: FS, r: any, edits: Edits) {
  const chosen = s.chosenFrom || {};
  for (const k of Object.keys(edits)) if (k in chosen) s.values[k] = chosen[k] ?? "";
  const typed = Object.fromEntries(Object.entries(edits).filter(([k]) => !(k in chosen)));
  s.removal = { r, edits, typed, draft: s.draft };
  overlay.set({ kind: "removal" });
  bump();
}

function confirmRemoval() {
  const s = cur();
  const held = s?.removal;
  if (!s || !held) return;
  s.removal = null;
  overlay.set(null);
  applyFill(s, held.r, held.edits);
  s.dirty = true;
  validate(s);
  const undo = () => undoRemoval(held.draft, s, undo);
  removalUndo = { state: s, undo };
  offerUndo(held.r.removes.done, undo);
  bump();
}

async function cancelRemoval() {
  const s = cur();
  const held = s?.removal;
  if (s) s.removal = null;
  overlay.set(null);
  if (s && held && Object.keys(held.typed).length) {
    const typed = held.typed;
    await serial(s, async () => {
      try {
        const r = await H.fill(s.code, s.draft, typed, unlockedFor(s, typed));
        if (r.removes) {
          const was = { ...s.values };
          for (const k of Object.keys(typed)) was[k] = "\0";
          syncChanged(s, was, {});
        } else {
          applyFill(s, r, typed);
          s.dirty = true;
          validate(s);
        }
      } catch (e) {
        s.pending = { ...typed, ...s.pending };
        toast(errText(e), "danger");
      }
    });
  }
  bump();
}

function retireRemovalUndo() {
  const held = removalUndo;
  if (!held) return;
  removalUndo = null;
  if (toastSig()?.undo === held.undo) toastSig.set(null);
}

function undoRemoval(draft: any, state: FS, undo: () => void) {
  const s = cur();
  if (!s || s !== state || removalUndo?.undo !== undo) return;
  removalUndo = null;
  serial(s, async () => {
    try {
      const r = await H.fill(s.code, draft, {}, []);
      applyFill(s, r, {});
    } catch (e) {
      toast(errText(e), "danger");
      return;
    }
    s.dirty = true;
    validate(s);
    bump();
  });
}

// ----- a sample's period (Form library)

function setPeriodPart(s: FS, parts: Edits) {
  s.periodValues = { ...s.periodValues, ...parts };
  for (const [k, v] of Object.entries(parts)) {
    s.values[k] = String(v);
    if (k in s.text) s.text[k] = String(v);
  }
  const asked = JSON.stringify(s.periodValues);
  if (asked === s.periodAsked) return bump();
  s.periodAsked = asked;
  closeOffer();
  s.opens = null;
  s.existing = null;
  const token = T;
  const seq = ++s.periodSeq;
  setTimeout(() => {
    if (!live(s, token) || seq !== s.periodSeq) return;
    askPeriod(s, asked);
  }, PERIOD_ASK_DELAY);
  bump();
}

async function askPeriod(s: FS, asked: string) {
  const token = T;
  s.periodInFlight = asked;
  let r: any;
  try {
    r = await H.samplePeriod(s.tin, s.code, JSON.parse(asked));
  } catch (e) {
    if (s.periodInFlight === asked) s.periodInFlight = null;
    if (token === T) toast(errText(e), "danger");
    return;
  }
  if (s.periodInFlight === asked) s.periodInFlight = null;
  if (!live(s, token) || asked !== s.periodAsked) return;
  s.periodAnswered = asked;
  await takePeriod(s, r);
  if (s.saveAfterAnswer) {
    const { then } = s.saveAfterAnswer;
    s.saveAfterAnswer = null;
    if (cur() === s) afterPeriodForSave(s, then);
  }
}

async function takePeriod(s: FS, r: any) {
  s.periodComplete = Boolean(r.complete);
  s.opens = r.complete ? r.opens || null : null;
  s.existing = r.existing || null;
  const period = ++s.periodToken;
  closeOffer();
  const offer = () => {
    if (r.existing) offerExisting(s, r.existing);
  };
  if (r.sample) {
    await rebuild(s, r.sample);
    offer();
  } else if (s.periodSet) {
    toPlaceholder(s, period, offer);
  } else offer();
  bump();
}

async function toPlaceholder(s: FS, period: number, then: (() => void) | null) {
  const token = T;
  let r: any;
  try {
    r = await H.sample(s.tin, s.code);
  } catch (e) {
    if (token === T) toast(errText(e), "danger");
    return;
  }
  if (!live(s, token) || period !== s.periodToken) return;
  await rebuild(s, r);
  then?.();
  bump();
}

function rebuild(s: FS, sample: any): Promise<void> {
  return serial(s, async () => {
    const before = s.values;
    const keepText = { ...s.text };
    takeOpened(s, sample);
    s.text = keepText;
    const typed = s.typedEdits;
    if (Object.keys(typed).length) {
      try {
        const f = await H.fill(s.code, s.draft, typed, unlockedFor(s, typed));
        s.draft = f.draft;
        s.values = shown(s, f.values || {});
        s.options = f.options || {};
        if (Array.isArray(f.notes_always)) s.notesAlways = new Set(f.notes_always);
      } catch (e) {
        toast(errText(e), "danger");
      }
    }
    syncComputed(s);
    syncChanged(s, before, { ...typed, ...s.periodValues });
    validate(s);
    bump();
  });
}

function closeOffer() {
  if (overlay()?.kind === "existing") overlay.set(null);
}

function offerExisting(s: FS, existing: any) {
  s.existing = existing;
  if (existing.status === "Draft" && fastLaneDrafts) return takeExisting("load", true);
  overlay.set({ kind: "existing", existing });
  bump();
}

function takeExisting(choice: string, auto = false) {
  const s = cur();
  const ov = overlay();
  const offered = auto ? null : ov?.kind === "existing" ? ov.existing : null;
  const existing = auto ? s?.existing : offered;
  closeOffer();
  if (!s || !s.sample || !existing || existing !== s.existing || !s.opens) return bump();
  if (choice === "keep") {
    const before = periodSnapshot(s);
    keepForm(s);
    return offerUndo("Kept this form; its period is cleared", () => restorePeriod(before, existing));
  }
  const sample = s;
  const { year, period, key } = s.opens;
  const opts: Opts = { tin: s.tin, code: s.code, year, period, ...(key != null ? { returnKey: key } : {}) };
  const what: Record<string, string> = {
    load: auto ? `Loaded your saved ${existing.label} draft (you chose not to be asked again)` : `Loaded your saved ${existing.label} draft`,
    view: `Opened ${existing.label} read-only`,
    amend: `Started an amended ${existing.label} return`,
  };
  const then = (error: string | null) => {
    if (error) {
      restoreForm(sample);
      return toast(`Could not open ${existing.label}: ${error}`, "danger");
    }
    offerUndo(what[choice], () => restoreForm(sample, offered));
  };
  openForm(choice === "amend" ? { ...opts, amend: true } : opts, then);
}

function restoreForm(sample: FS, offer: any = null) {
  const s = cur();
  if (!sample || s === sample) return;
  if (s && (s.dirty || Object.keys(s.pending).length)) {
    return toast("Undo would lose edits you have not saved. Save them first, or leave this return.", "danger");
  }
  const savedAmendment = Boolean(s?.amends && s.saved);
  cur.set(sample);
  T++;
  view.set("form");
  if (offer && sample.existing === offer) overlay.set({ kind: "existing", existing: offer });
  if (savedAmendment) toast("Back to the sample. The amended return was saved and stays saved.");
  bump();
}

function periodSnapshot(s: FS) {
  return {
    state: s,
    fields: {
      draft: s.draft, values: { ...s.values }, options: s.options, locks: s.locks, lockNotes: s.lockNotes, notesAlways: s.notesAlways,
      blank: s.blank, needs: s.needs, whys: s.whys, errors: s.errors, messages: s.messages, formErrors: s.formErrors,
      periodValues: { ...s.periodValues }, periodAsked: s.periodAsked, periodSet: s.periodSet, opens: s.opens, existing: s.existing,
    },
    text: { ...s.text },
  };
}

function restorePeriod(snapshot: ReturnType<typeof periodSnapshot>, offer: any) {
  const s = cur();
  if (!s || s !== snapshot.state) return;
  s.periodToken++;
  s.periodSeq++;
  Object.assign(s, snapshot.fields);
  s.text = { ...snapshot.text };
  if (offer && s.existing === offer) overlay.set({ kind: "existing", existing: offer });
  bump();
}

function keepForm(s: FS) {
  s.periodValues = {};
  s.periodAsked = JSON.stringify({});
  s.periodAnswered = null;
  s.periodComplete = false;
  s.periodSeq++;
  s.opens = null;
  s.existing = null;
  for (const k of s.periodParts.keys()) {
    s.values[k] = "";
    if (k in s.text) s.text[k] = "";
  }
  if (s.periodSet) toPlaceholder(s, ++s.periodToken, null);
  bump();
}

// ----- profile boxes, penalties, from-hints

function itemEditKeys(s: FS, k: string) {
  const id = itemIdForKey(s, k);
  if (!id || !s.schema) return [k];
  const [pi, ii] = id.split(":").map(Number);
  return (s.schema.parts[pi].items[ii].f || []).map((f) => f.k).filter((key) => s.profileEdit.has(key));
}
function endProfileEdit(s: FS, keys: string[]) {
  let changed = false;
  for (const k of keys) changed = s.editing.delete(k) || changed;
  if (changed) bump();
  return changed;
}
function startProfileEdit(s: FS, keys: string[]) {
  if (!s.editable || !s.schema) return;
  const id = keys.length ? itemIdForKey(s, keys[0]) : null;
  endProfileEdit(s, [...s.editing].filter((k) => itemIdForKey(s, k) !== id));
  if (id) {
    const [pi, ii] = id.split(":").map(Number);
    if (s.schema.parts[pi].items[ii].l === "Additional details") s.moreOpen.add(id);
  }
  for (const k of keys) {
    if (!s.profileEdit.has(k)) continue;
    s.editing.add(k);
    s.opened.add(k);
  }
  bump();
}
function enterBox(s: FS, k: string) {
  s.focusKey = k;
  const here = itemIdForKey(s, k);
  endProfileEdit(s, [...s.editing].filter((key) => itemIdForKey(s, key) !== here));
}
function leaveBox(s: FS, k: string) {
  if (s.focusKey === k) s.focusKey = null;
  if (!itemEditKeys(s, k).some((key) => s.editing.has(key))) return;
  const token = T;
  setTimeout(() => {
    if (!live(s, token)) return;
    const now = s.focusKey;
    if (now && itemIdForKey(s, now) === itemIdForKey(s, k)) return;
    endProfileEdit(s, itemEditKeys(s, k));
  }, 50);
}

function openPenalties(s: FS, g: Pen) {
  if (!s.editable || !g.applies) return;
  for (const k of g.keys) s.penaltyOpen.add(k);
  bump();
}
function usePenaltyComputed(s: FS, g: Pen) {
  if (g.edited || g.kept) choose(s, Object.fromEntries(g.keys.map((k) => [k, ""])));
  for (const k of g.keys) s.penaltyOpen.delete(k);
  bump();
}
function useFrom(s: FS, from: NonNullable<Item["from"]>) {
  const pesos = wholePesosOf(s.values[from.k]);
  if (!s.editable || isLocked(s, from.to) || !pesos) return;
  const text = formatMoney(String(pesos.amount), true);
  s.text[from.to] = text;
  s.pending[from.to] = text;
  s.dirty = true;
  unquiet(s, [from.to]);
  flush(s);
}

async function useProfileValues(s: FS) {
  if (!s.draft || !s.profileDiffers.length) return;
  await flush(s);
  const ok = await serial(s, async () => {
    const before = s.values;
    try {
      const r = await H.useProfile(s.code, s.year, s.period, s.draft);
      s.draft = r.draft;
      s.values = shown(s, r.values || {});
      s.options = r.options || s.options;
      if (r.pools) s.poolTexts = r.pools;
      if (Array.isArray(r.penalties)) s.penalties = r.penalties;
    } catch (e) {
      toast(errText(e), "danger");
      return false;
    }
    s.profileDiffers = [];
    s.dirty = true;
    syncComputed(s);
    syncChanged(s, before, {});
    return true;
  });
  if (!ok) return;
  if (s.amends && !s.saved) return bump();
  await save(s);
}

async function markChecked(s: FS, token: string) {
  if (!s.draft) return;
  try {
    await H.checkNotice(s.code, s.draft, token);
  } catch (e) {
    return toast(errText(e), "danger");
  }
  validate(s);
}

async function check(s: FS) {
  unquiet(s);
  await flush(s);
  const token = T;
  const wait = () => {
    if (!live(s, token)) return;
    if (s.checking) return setTimeout(wait, 100);
    const n = s.errors.length;
    toast(n ? `${n} item${n > 1 ? "s need" : " needs"} attention.` : "No problems found.", n ? "danger" : "success");
  };
  setTimeout(wait, 50);
}

// ----- save, queue, print

async function save(s: FS): Promise<boolean> {
  if (!s.draft) return false;
  if (s.sample) {
    saveSample(s);
    return false;
  }
  unquiet(s);
  const token = T;
  return serial(s, async () => {
    let edits = s.pending;
    s.pending = {};
    try {
      if (Object.keys(edits).length) {
        retireRemovalUndo();
        const f = await H.fill(s.code, s.draft, edits, unlockedFor(s, edits));
        if (f.removes) {
          holdRemoval(s, f, edits);
          return false;
        }
        accepted(s, edits, f.rejected);
        s.draft = f.draft;
        s.values = shown(s, f.values || {});
        if (f.pools) s.poolTexts = f.pools;
        if (Array.isArray(f.penalties)) s.penalties = f.penalties;
        edits = {};
      }
      const r = await H.save(s.code, s.draft);
      retireRemovalUndo();
      if (!live(s, token)) return false;
      s.draft = r.draft;
      s.status = r.status;
      s.dirty = false;
      s.saved = true;
      toast("Draft saved.", "success");
      validate(s);
      return true;
    } catch (e) {
      s.pending = { ...edits, ...s.pending };
      toast(errText(e), "danger");
      return false;
    } finally {
      bump();
    }
  });
}

/** Save on a Form library sample: it becomes the return for its period. */
async function saveSample(s: FS, then: (() => void) | null = null) {
  if (s.periodAsked && s.periodAnswered !== s.periodAsked && Object.keys(s.periodValues).length) {
    s.saveAfterAnswer = { then };
    if (s.periodInFlight !== s.periodAsked) {
      s.periodSeq++;
      askPeriod(s, s.periodAsked);
    }
    return;
  }
  if (s.existing) return offerExisting(s, s.existing);
  if (!s.periodSet) return askSamplePeriod(s, then);
  const r = await serial(s, async () => {
    let edits = s.pending;
    s.pending = {};
    try {
      if (Object.keys(edits).length) {
        const f = await H.fill(s.code, s.draft, edits, unlockedFor(s, edits));
        accepted(s, edits, f.rejected);
        s.draft = f.draft;
        s.values = shown(s, f.values || {});
        if (f.pools) s.poolTexts = f.pools;
        edits = {};
      }
      return await H.save(s.code, s.draft);
    } catch (e) {
      s.pending = { ...edits, ...s.pending };
      toast(errText(e), "danger");
      return null;
    }
  });
  if (!r) return;
  if (!r.saved) {
    s.opens = r.opens || null;
    return offerExisting(s, r.existing);
  }
  s.dirty = false;
  toast("Saved. The sample is now your return for this period.", "success");
  const { year, period, key } = r.opens;
  openForm({ tin: s.tin, code: s.code, year, period, ...(key != null ? { returnKey: key } : {}) }, then ? (error) => {
    if (!error) then();
  } : null);
}

function afterPeriodForSave(s: FS, then: (() => void) | null) {
  if (s.existing) return;
  if (!s.periodComplete) return askSamplePeriod(s, then, "Enter the whole period to save this sample.");
  saveSample(s, then);
}

function askSamplePeriod(s: FS, then: (() => void) | null, error = "") {
  const values: Edits = {};
  let quarter: string | null = null;
  for (const [k, p] of s.periodParts) {
    const v = String(s.periodValues[k] ?? s.values[k] ?? "");
    if (p.part === "quarter") {
      if (v === "true") quarter = k;
    } else values[k] = v;
  }
  overlay.set({ kind: "sample-period", ask: { state: s, then, values, quarter, error } });
}

function takeSamplePeriod(ask: SamplePeriodAsk) {
  const s = cur();
  overlay.set(null);
  if (!s || ask.state !== s) return;
  const parts: Edits = {};
  for (const [k, p] of s.periodParts) parts[k] = p.part === "quarter" ? String(ask.quarter === k) : (ask.values[k] ?? "").trim();
  setPeriodPart(s, parts);
  if (s.periodAnswered === s.periodAsked) return afterPeriodForSave(s, ask.then);
  s.saveAfterAnswer = { then: ask.then };
  s.periodSeq++;
  askPeriod(s, s.periodAsked!);
}

const usesPeriodPath = (s: FS) => s.code === "1601C" || s.code === "2551Q";

async function ensureSaved(s: FS): Promise<boolean> {
  if (s.sample) {
    saveSample(s);
    return false;
  }
  if (!s.dirty && s.saved && !Object.keys(s.pending).length) return true;
  await save(s);
  return s.saved && !s.dirty;
}

function reload(s: FS) {
  openForm({ tin: s.tin, code: s.code, year: s.year, period: s.period, sample: s.sample, viewOnly: s.viewOnly, ...(s.returnKey != null ? { returnKey: s.returnKey } : {}) });
}

// ----- the assistant: its standing OK to submit, and the hand-off to a chat

const grantTin = (s: FS) => (digits(s.tin).length === 9 ? `${digits(s.tin)}00000` : digits(s.tin));

/** Reads whether the assistant may submit this form for this taxpayer. */
async function readGrant(s: FS, token: number) {
  if (s.sample) return;
  try {
    const grants = await H.grants(grantTin(s));
    if (!live(s, token)) return;
    s.assistantMay = grants.some((g) => digits(g.tin) === grantTin(s) && normCode(g.form) === s.code);
  } catch {
    if (live(s, token)) s.assistantMay = null;
  }
  bump();
}

/** The toggle: grants or revokes the assistant's standing OK. The user's own Submit never needs it. */
async function setAssistantMay(s: FS, may: boolean) {
  try {
    if (may) await H.grant(grantTin(s), s.code);
    else await H.revoke(grantTin(s), s.code);
    s.assistantMay = may;
    toast(may ? `The assistant may now submit ${s.code} for this taxpayer.` : `The assistant can no longer submit ${s.code} for this taxpayer.`, "success");
  } catch (e) {
    toast(errText(e), "danger");
  }
  bump();
}

const pad2 = (n: number) => String(n).padStart(2, "0");

/** The return's period as a token writes it: `2026-01`, `2026-Q3`, `2025`, `2026-10-26`. */
function tokenPeriod(s: FS): string {
  const freq = s.schema?.freq || "";
  if (freq === "Monthly" && s.period >= 1 && s.period <= 12) return `${s.year}-${pad2(s.period)}`;
  if (freq === "Quarterly" && s.period >= 1 && s.period <= 4) return `${s.year}-Q${s.period}`;
  if (freq === "As needed") {
    // The transaction's day when the return is keyed by it (`MMDD`), else today.
    const key = Number(s.returnKey ?? 0);
    const day = new Date(s.year, Math.floor(key / 100) - 1, key % 100);
    if (key >= 101 && key <= 1231 && day.getMonth() === Math.floor(key / 100) - 1) return `${s.year}-${pad2(Math.floor(key / 100))}-${pad2(key % 100)}`;
    const now = new Date();
    return `${now.getFullYear()}-${pad2(now.getMonth() + 1)}-${pad2(now.getDate())}`;
  }
  return String(s.year);
}

/** "Ask the assistant": this return attached to the chat (Rox asks the user first). */
function askAssistant(s: FS) {
  attachReturn(s)
    .then(() => toast("Attached to the chat: ask the assistant about this return there.", "success"))
    .catch((e) => toast(errText(e), "danger"));
}

async function queue(s: FS) {
  if (s.sample) return saveSample(s, () => {
    const now = cur();
    if (now) queue(now);
  });
  if (!(await ensureSaved(s))) return;
  try {
    if (usesPeriodPath(s)) await H.queuePeriod(s.tin, s.code, s.year, s.period || null);
    else await H.queue(s.code, s.draft);
    toast("Queued for background submission.", "success");
    reload(s);
  } catch (e) {
    toast(errText(e), "danger");
  }
}

async function cancelQueue(s: FS) {
  if (s.sample) return toast(SAMPLE_NOTE);
  try {
    if (usesPeriodPath(s)) await H.cancelQueued(s.tin, s.code, s.year, s.period || null);
    else await H.cancelQueue(s.code, s.draft);
    toast("Queue cancelled. The return is an editable draft again.");
    reload(s);
  } catch (e) {
    toast(errText(e), "danger");
  }
}

/** Print preview: BIR fills the return's print sheet (`bir_app_print_preview`) and Rox shows it over this app
 *  with Print (the macOS print panel, which also saves a PDF) and Close; the page never comes through here. A
 *  sample previews as the page holds it (staged edits, held boxes blank); a return is saved first and prints as
 *  stored; a sent or queued return shows its filing record. A Rox without `rox.preview` gets today's note. */
async function print(s: FS) {
  s.printResult = null;
  if (!rox.preview) {
    s.printResult = { sample: !!s.sample, error: "Print preview is not available in Rox yet. Print this return from BIR on the desktop, or ask the assistant for its filing record." };
    return bump();
  }
  const preview = (args: Record<string, unknown>) => rox.preview({ tool: "bir_app_print_preview", args: { ticket: BRANCH, form: s.code, ...args } });
  try {
    if (s.sample) {
      const pending = s.pending;
      // as text: a draft's amounts are not integers, and a signed call carries no other numbers
      await preview({ draft_json: JSON.stringify(s.draft), edits: pending, unlocked: unlockedFor(s, pending) });
      return bump();
    }
    if (s.amends && !s.saved) await flush(s);
    else if (s.editable && !(await ensureSaved(s))) return;
    // 2551Q and 1601C are kept by period; the others by their id
    await preview(sentOrQueued(s) ? { period: tokenPeriod(s) } : usesPeriodPath(s) ? { year: s.year, quarter: s.period } : { draft_id: Number(s.draft.id) });
  } catch (e) {
    s.printResult = { sample: !!s.sample, error: errText(e) };
  }
  bump();
}

// ----- payment and the filing record

/** Whether this return has left Draft: it can be paid and has a filing record. */
const sentOrQueued = (s: FS) => !s.sample && !s.filedOriginalView && s.status !== "Draft";

async function readPayments(s: FS, token: number) {
  if (!sentOrQueued(s)) return;
  try {
    const r = await H.payments(s.code, tokenPeriod(s));
    if (!live(s, token)) return;
    s.payments = r.payments || [];
    s.paymentsError = null;
  } catch (e) {
    if (live(s, token)) {
      s.payments = null;
      s.paymentsError = errText(e);
    }
  }
  bump();
}

/** Remove a payment recorded on this return (on the wrong return, or a test), after the row's own confirm. */
async function removePayment(s: FS, id: string) {
  s.paying = true;
  bump();
  try {
    await H.removePayment(s.code, tokenPeriod(s), id);
    toast("Payment removed.", "success");
  } catch (e) {
    toast(errText(e), "danger");
  }
  s.removeAsk = null;
  s.paying = false;
  await readPayments(s, T);
}

/** The return's receipt (every payment and its proof: the receipt file or the payment email) in Rox's document
 *  view, made by BIR (`bir_payment_receipt`). */
function viewReceipt(s: FS) {
  rox.preview({ tool: "bir_payment_receipt", args: { tin: BRANCH, form: s.code, period: tokenPeriod(s) } }).catch((e) => toast(errText(e), "danger"));
}

/** The payment email in Gmail, for a payment recorded from one ("" otherwise). */
const gmailOf = (s: FS) => {
  const id = (s.payments || []).map((p) => p.message_id).find(Boolean);
  return id ? `https://mail.google.com/mail/u/0/#all/${encodeURIComponent(id)}` : "";
};

const pesos = (n: number | null) => (n == null ? "" : `PHP ${n.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`);

function paymentLine(p: Payment): string {
  if (p.channel === "manual") return `Marked paid${p.proof.kind === "file" ? ", receipt attached" : ", no receipt"}`;
  const paid = p.total ?? p.amount;
  return [`Paid ${pesos(paid)}`.trim(), `via ${p.channel}`, p.paid_at ? `on ${shortDate(p.paid_at)}` : "", p.reference ? `ref ${p.reference}` : "", p.proof.kind === "file" ? "receipt attached" : p.proof.kind === "email" ? "from its email" : ""]
    .filter(Boolean)
    .join(" · ");
}

/** "Mark as paid": the payment's details when the user gives them (date, amount, channel such as MyEG,
 *  reference: `bir_record_payment`), else on the user's word alone (`bir_mark_paid`). A receipt can be
 *  attached later. */
async function markPaid(s: FS) {
  const d = s.payForm;
  s.paying = true;
  bump();
  try {
    if (d && d.amount.trim() && d.channel.trim()) {
      await H.recordPayment(s.code, tokenPeriod(s), { ...d, paid_at: d.paid_at.trim() || isoToday() });
      toast("Payment recorded. Attach the receipt when you have it.", "success");
    } else {
      await H.markPaid(s.code, tokenPeriod(s));
      toast("Marked as paid. Attach the receipt when you have it.", "success");
    }
    s.payForm = null;
  } catch (e) {
    toast(errText(e), "danger");
  }
  s.paying = false;
  await readPayments(s, T);
}

/** Today as YYYY-MM-DD (the payment date's default). */
function isoToday() {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/**
 * "Attach receipt": Rox's file picker, and bir keeps a copy of the image or PDF as this return's proof of payment.
 * When the picker fails, the return is attached to the chat: the user drops the receipt there and the assistant records it.
 */
async function attachReceipt(s: FS) {
  let proof: { upload: string; name: string } | null;
  s.paying = true;
  bump();
  try {
    proof = await pickReceipt();
  } catch (e) {
    s.paying = false;
    bump();
    if ((e as any)?.code === "quota_exceeded") return toast("The receipt is over 25 MB; attach a smaller scan.", "danger");
    attachReturn(s)
      .then(() => toast("Attached to the chat: drop the receipt there and send it; the assistant records it."))
      .catch((err) => toast(errText(err), "danger"));
    return;
  }
  if (!proof) {
    s.paying = false;
    return bump();
  }
  if (!/\.(png|jpe?g|gif|webp|heic|pdf)$/i.test(proof.name)) {
    s.paying = false;
    bump();
    return toast("A receipt must be an image or a PDF.", "danger");
  }
  try {
    await H.markPaid(s.code, tokenPeriod(s), proof);
    toast("Receipt attached as proof of payment.", "success");
  } catch (e) {
    toast(errText(e), "danger");
  }
  s.paying = false;
  await readPayments(s, T);
}

function openFiledOriginal(s: FS) {
  if (!s.filedOriginal) return;
  if (s.dirty || Object.keys(s.pending).length) return toast("Save your edits first: viewing the filed original leaves this return.", "danger");
  const back: Opts = { tin: s.tin, code: s.code, year: s.year, period: s.period, ...(s.returnKey != null ? { returnKey: s.returnKey } : {}) };
  openForm({ tin: s.tin, code: s.code, year: s.year, period: s.period, filedOriginalId: s.filedOriginal.id, back });
}

// ----- parts and the date picker

function togglePart(s: FS, i: number) {
  if (s.openParts.has(i)) s.openParts.delete(i);
  else s.openParts.add(i);
  bump();
}
let formScroll: YNode | undefined;
function jumpTo(s: FS, i: number) {
  s.openParts = new Set([i]);
  bump();
  setTimeout(() => formScroll?.scrollTo({ y: 0, animated: true }), 30);
}

function openPicker(s: FS, id: string, date: DateSpec) {
  if (s.picker?.id === id) {
    s.picker = null;
    return bump();
  }
  const typed = date.single ? parseTypedDate(s.values[date.single.k]) : null;
  const m = typed ? typed.m : date.single ? 0 : Number(String(s.values[date.month!.k] || "").slice(0, 2));
  const y = typed ? typed.y : date.single ? 0 : Number(s.values[date.year!.k] || "");
  const d = typed ? typed.d : date.day ? Number(s.values[date.day.k] || "") : 1;
  const now = new Date();
  const ok = m >= 1 && m <= 12 && y > 1900;
  const sel = ok ? `${y}-${String(m).padStart(2, "0")}-${String(d >= 1 && d <= 31 ? d : 1).padStart(2, "0")}` : "";
  s.picker = { id, date, y: ok ? y : now.getFullYear(), m: ok ? m : now.getMonth() + 1, sel, list: null };
  bump();
}

function pickDate(s: FS, date: DateSpec, iso: string) {
  const [y, m, d] = iso.split("-");
  if (date.single) {
    const f = date.single;
    if (f.c || isLocked(s, f.k)) return;
    const text = formatMDY({ y: Number(y), m: Number(m), d: Number(d) });
    s.text[f.k] = shownDate(text);
    setDateError(s, f.k, "");
    s.picker = null;
    choose(s, { [f.k]: text });
    return;
  }
  const value = (f: Field, v: string): string | null => {
    if (!(f.t === "sel" && f.o)) return v;
    const listed = f.o.map(optionOf).filter(Boolean) as Opt[];
    if (!listed.length) return v;
    const hit = listed.find((o) => o.value === v || o.value === String(Number(v)) || (f === date.year && o.label === String(Number(v))));
    return hit ? hit.value : null;
  };
  const parts = ([[date.month, m], [date.day, d], [date.year, y]] as [Field | null | undefined, string][]).filter(
    (x): x is [Field, string] => Boolean(x[0]) && !x[0]!.c && !isLocked(s, x[0]!.k),
  );
  const edits: Edits = {};
  for (const [f, v] of parts) {
    const chosen = value(f, v);
    if (chosen === null) return toast("That date is not on this form's list.", "danger");
    edits[f.k] = chosen;
  }
  for (const [k, v] of Object.entries(edits)) if (k in s.text) s.text[k] = String(v);
  if (date.oneKey) {
    s.text[date.oneKey] = formatLongDate({ y: Number(y), m: Number(m), d: Number(d) });
    setDateError(s, date.month!.k, "");
  }
  s.picker = null;
  choose(s, edits);
}

// ----- typing into a box

function typeInto(s: FS, k: string, raw: string) {
  s.text[k] = raw;
  const od = s.idx?.oneDates.get(k);
  if (od) {
    const keys = [od.month!.k, od.day?.k || "", od.year!.k];
    const typed = parseTypedDate(raw);
    if (typed || !raw.trim()) setDateError(s, keys[0], "");
    if (!typed && raw.trim()) return;
    const [mm, dd, yy] = typed ? formatMDY(typed).split("/") : ["", "", ""];
    [mm, dd, yy].forEach((v, i) => keys[i] && edit(s, keys[i], v));
    return;
  }
  if (kindOf(s, k) === "date") {
    const date = parseTypedDate(raw);
    s.datesEdited.add(k);
    if (date || !raw.trim()) setDateError(s, k, "");
    edit(s, k, date ? formatMDY(date) : raw);
    return;
  }
  const pool = poolOf(s, k);
  edit(s, pool ? pool.keys[0] : k, raw);
}

function blurBox(s: FS, k: string) {
  const raw = textOf(s, k);
  const od = s.idx?.oneDates.get(k);
  const kind = kindOf(s, k);
  if (od || kind === "date") {
    const date = parseTypedDate(raw);
    const errKey = od ? od.month!.k : k;
    if (date) {
      s.text[k] = formatLongDate(date);
      setDateError(s, errKey, "");
    } else setDateError(s, errKey, raw.trim() && (od || s.datesEdited.has(k)) ? DATE_ERROR : "");
    leaveBox(s, od ? od.month!.k : k);
  } else {
    if (kind === "money" && k in s.text) s.text[k] = formatMoney(raw, isWhole(s, k));
    leaveBox(s, k);
  }
  flush(s);
  bump();
}

// =====================================================================
// Small views
// =====================================================================

function Btn(p: { label: string; onPress: () => void; variant?: "primary" | "secondary" | "ghost" | "danger"; disabled?: boolean; small?: boolean }) {
  const box = () => {
    const v = p.variant || "secondary";
    const size = p.small === false ? "px-4 py-2" : "px-3 py-1.5";
    const tone = v === "primary" ? C.accentBg : v === "danger" ? "bg-[#c62828] dark:bg-[#e05555]" : v === "ghost" ? "" : C.inset;
    return `rounded-md items-center justify-center ${size} ${tone} ${p.disabled ? "opacity-40" : ""}`;
  };
  const label = () => {
    const v = p.variant || "secondary";
    return `text-sm font-semibold ${v === "primary" || v === "danger" ? "text-white dark:text-white" : v === "ghost" ? C.accent : C.fg}`;
  };
  return (
    <View class={box()} activeStyle={{ opacity: 0.7 }} onPress={() => !p.disabled && p.onPress()}>
      <Text class={label()}>{p.label}</Text>
    </View>
  );
}

function Chip(p: { text: string; tone?: "neutral" | "accent" | "success" | "warning" | "danger" }) {
  const cls = () => {
    const t = p.tone || "neutral";
    if (t === "accent") return [C.accentSoft, C.accent];
    if (t === "success") return ["bg-[#2e7d321f] dark:bg-[#66bb6a33]", "text-[#2e7d32] dark:text-[#81c784]"];
    if (t === "warning") return ["bg-[#ef6c0024] dark:bg-[#ffb74d33]", "text-[#b45309] dark:text-[#ffb74d]"];
    if (t === "danger") return [C.dangerSoft, C.danger];
    return [C.inset, C.fg2];
  };
  return (
    <View class={`rounded-full px-2 py-0.5 ${cls()[0]}`}>
      <Text class={`text-xs font-medium ${cls()[1]}`}>{p.text}</Text>
    </View>
  );
}

function statusTone(status: string): "neutral" | "accent" | "success" | "warning" {
  if (status === "Draft") return "warning";
  if (status === "Queued") return "accent";
  if (status === "Submitted" || status === "Confirmed" || status === "Paid") return "success";
  return "neutral";
}

function LockedBox(p: { text: string; width: number; hint?: boolean; right?: boolean; center?: boolean }) {
  return (
    <View
      class={`h-8 px-2.5 rounded-md border ${C.line} ${C.inset} justify-center`}
      style={{ width: p.width, alignItems: p.right ? "flex-end" : p.center ? "center" : "flex-start" }}
    >
      <Text class={`text-sm ${p.hint ? C.fg3 : C.fg2}`} numberOfLines={1}>{p.text}</Text>
    </View>
  );
}

function ErrorBox(p: { text: string; onRetry?: () => void }) {
  return (
    <View class={`p-3 rounded-xl gap-2 ${C.dangerSoft}`}>
      <Text class={`text-sm ${C.danger}`}>{p.text}</Text>
      <If when={Boolean(p.onRetry)}>
        <View class="flex-row">
          <Btn label="Retry" onPress={() => p.onRetry?.()} />
        </View>
      </If>
    </View>
  );
}

// =====================================================================
// Field controls (control / selectBox / radioGroup / tinBoxes / scheduleTable)
// =====================================================================

/** An input box bound to key `k` (a field, a schedule cell, a pooled address, a one-date box). */
function Box(p: { s: FS; k: string; t: string; width: number; label: string; maxLength?: number }) {
  const s = p.s;
  const kb = p.t === "money" || p.t === "pct" ? "decimal-pad" : p.t === "int" ? "number-pad" : p.t === "email" ? "email-address" : p.t === "phone" ? "phone-pad" : "default";
  const input = (
    <Input
      class={C.input}
      style={{ width: p.t === "money" || p.t === "pct" ? p.width - 22 : p.width, textAlign: p.t === "money" || p.t === "pct" ? "right" : "left" }}
      value={(rev(), textOf(s, p.k))}
      placeholder={p.t === "date" ? "MM/DD/YYYY" : ""}
      keyboardType={kb}
      autoCapitalize="none"
      autoCorrect={false}
      maxLength={p.t === "text" || p.t === "int" ? p.maxLength : undefined}
      readonly={(rev(), !s.editable)}
      onChange={(e) => typeInto(s, p.k, typed(e))}
      onFocus={() => enterBox(s, p.k)}
      onBlur={() => blurBox(s, p.k)}
    />
  );
  if (p.t !== "money" && p.t !== "pct") return input;
  return (
    <View class="flex-row items-center gap-1" style={{ width: p.width }}>
      <If when={p.t === "money"}>
        <Text class={`text-xs ${C.fg3}`} style={{ width: 14 }}>₱</Text>
      </If>
      {input}
      <If when={p.t === "pct"}>
        <Text class={`text-xs ${C.fg3}`} style={{ width: 14 }}>%</Text>
      </If>
    </View>
  );
}

const selectQuery = signal("");

/** A dropdown for key `k`: the host's catalog choices or the schema's listed ones. */
function SelectBox(p: { s: FS; k: string; label: string; listed?: any[]; width: number }) {
  const s = p.s;
  const k = p.k;
  const model = computed(() => {
    rev();
    const listed = ((s.options[k] || p.listed || []) as any[]).map(optionOf).filter(Boolean) as Opt[];
    const value = String(s.values[k] ?? "");
    const options: Opt[] = [{ value: "", label: "Choose…" }, ...listed];
    const selected = listed.find((o) => o.selected);
    const known = options.some((o) => o.value === value);
    const shownV = selected ? selected.value : known ? value : /^(0+|-)$/.test(value) ? "" : value;
    if (shownV && !selected && !known) options.push({ value: shownV, label: shownV });
    const locked = !s.editable || Boolean(s.idx?.computed.has(k)) || isLocked(s, k);
    const label = (options.find((o) => o.value === shownV) || { label: shownV }).label;
    const hint = locked && !shownV ? reasonFor(s, k) : "";
    return { options, shownV, locked, label, hint, open: s.openSelect === k };
  });
  const filtered = () => {
    const q = selectQuery().trim().toLowerCase();
    const opts = model().options;
    return q ? opts.filter((o) => o.label.toLowerCase().includes(q)) : opts;
  };
  const toggle = () => {
    s.openSelect = s.openSelect === k ? null : k;
    selectQuery.set("");
    bump();
  };
  return (
    <If
      when={!model().locked}
      else={<LockedBox text={model().hint || model().label} hint={Boolean(model().hint)} width={p.width} />}
    >
      <View class="gap-1" style={{ width: p.width }}>
        <View
          class={`h-8 px-2.5 rounded-md border ${C.line} ${C.pane} flex-row items-center justify-between`}
          activeStyle={{ opacity: 0.7 }}
          onPress={toggle}
        >
          <Text class={`text-sm flex-1 ${model().shownV ? C.fg : C.fg3}`} numberOfLines={1}>{model().label || "Choose…"}</Text>
          <Text class={`text-xs ${C.fg3}`}>{model().open ? "▲" : "▼"}</Text>
        </View>
        <If when={model().open}>
          <View class={`rounded-md border ${C.line} ${C.card} p-1 gap-0.5`}>
            <If when={model().options.length > 12}>
              <Input class={C.input} placeholder={`Find in ${p.label}`} value={selectQuery()} onChange={(e) => selectQuery.set(typed(e))} autoCapitalize="none" autoCorrect={false} />
            </If>
            <Scroll style={{ maxHeight: 280 }}>
              <Each of={filtered()} key={(o) => o.value}>
                {(o) => (
                  <View
                    class={`px-2 py-1.5 rounded ${o().value === model().shownV ? C.accentSoft : ""}`}
                    activeStyle={{ opacity: 0.6 }}
                    onPress={() => {
                      s.openSelect = null;
                      choose(s, { [k]: o().value });
                    }}
                  >
                    <Text class={`text-sm ${C.fg}`}>{o().label}</Text>
                  </View>
                )}
              </Each>
            </Scroll>
          </View>
        </If>
      </View>
    </If>
  );
}

function CheckBox(p: { on: boolean; locked?: boolean }) {
  return (
    <View
      class={`rounded items-center justify-center border ${p.on ? "border-[#1680f3] dark:border-[#1884fc] " + C.accentBg : C.line + " " + C.pane}`}
      style={{ width: 18, height: 18, opacity: p.locked ? 0.7 : 1 }}
    >
      <If when={p.on}>
        <Text class="text-xs font-bold text-white dark:text-white">✓</Text>
      </If>
    </View>
  );
}

function ChkField(p: { s: FS; f: Field; itemLabel: string }) {
  const s = p.s;
  const f = p.f;
  const on = () => (rev(), ["true", "1", "Y"].includes(String(s.values[f.k] ?? "")));
  const locked = () => (rev(), !s.editable || Boolean(f.c) || isLocked(s, f.k));
  const hint = () => (locked() && !on() ? reasonFor(s, f.k) : "");
  return (
    <View
      class="flex-row items-center gap-2 flex-wrap"
      style={{ opacity: (rev(), isOff(s, f.k) ? OFF_OPACITY : 1) }}
      activeStyle={{ opacity: 0.7 }}
      onPress={() => !locked() && choose(s, { [f.k]: on() ? "false" : "true" })}
    >
      <CheckBox on={on()} locked={locked()} />
      <Text class={`text-sm ${locked() ? C.fg2 : C.fg}`}>{f.l || p.itemLabel}</Text>
      <If when={Boolean(hint())}>
        <Text class={`text-xs ${C.fg3}`}>{hint()}</Text>
      </If>
    </View>
  );
}

/** One field of an item (control() in form_page.js). */
function FieldControl(p: { s: FS; f: Field; itemLabel: string; multi: boolean }) {
  const s = p.s;
  const f = p.f;
  if (f.t === "chk") return <ChkField s={s} f={f} itemLabel={p.itemLabel} />;
  const width = controlWidth(f.t, f.m);
  const showSub = p.multi && f.l && f.l !== p.itemLabel && !/^\d+[A-Z]?$/.test(f.l);
  const locked = () => (rev(), !s.editable || Boolean(f.c) || isLocked(s, f.k));
  const lockedText = () => {
    rev();
    const value = f.pool ? poolText(s, f) : String(s.values[f.k] ?? "");
    const sh = f.t === "money" ? formatMoney(value || (isComputedBox(s, f.k) || penaltyGroupOf(s, f.k) ? "0" : ""), f.w) : f.t === "date" ? shownDate(value) : value;
    const hint = !sh ? reasonFor(s, f.k) : "";
    return { text: sh || hint, hint: Boolean(hint), right: !hint && (f.t === "money" || f.t === "pct") };
  };
  return (
    <View class="gap-1" style={{ opacity: (rev(), isOff(s, f.k) ? OFF_OPACITY : 1), maxWidth: width + 4 }}>
      <If when={Boolean(showSub)}>
        <Text class={`text-xs ${C.fg2}`}>{f.l}</Text>
      </If>
      <If when={f.t === "sel"} else={
        <If when={!locked()} else={<LockedBox text={lockedText().text} hint={lockedText().hint} right={lockedText().right} width={width} />}>
          <Box s={s} k={f.k} t={f.t} width={width} label={f.l} maxLength={f.m} />
        </If>
      }>
        <SelectBox s={s} k={f.k} label={f.l} listed={f.o} width={width} />
      </If>
    </View>
  );
}

/** A one-answer group drawn as the paper form's tick boxes. */
function RadioGroup(p: { s: FS; radios: Field[] }) {
  const s = p.s;
  const radios = p.radios;
  const wide = radios.some((f) => choiceLabel(f).length > 24);
  const hint = () => {
    rev();
    const shut = radios.every((f) => !s.editable || f.c || isLocked(s, f.k)) && !radios.some((f) => s.values[f.k] === "true");
    return shut ? radios.map((f) => reasonFor(s, f.k)).find(Boolean) || "" : "";
  };
  const pick = (f: Field) => {
    const on = s.values[f.k] === "true";
    const locked = !s.editable || f.c || isLocked(s, f.k);
    if (locked) return;
    const period = s.periodParts.has(f.k);
    if (on && !period && (f.rg || 0) >= 3) return choose(s, { [f.k]: "false" });
    if (on && !period && !f.rg) return;
    const edits: Edits = { [f.k]: "true" };
    if (f.grp) for (const r of radios) if (r.k !== f.k && r.grp === f.grp) edits[r.k] = "false";
    choose(s, edits);
  };
  return (
    <View class={wide ? "gap-2" : "flex-row flex-wrap"} style={wide ? {} : { columnGap: 16, rowGap: 8 }}>
      <Each of={radios}>
        {(f) => {
          const on = () => (rev(), s.values[f().k] === "true");
          const locked = () => (rev(), !s.editable || Boolean(f().c) || isLocked(s, f().k));
          return (
            <View
              class="flex-row items-center gap-2"
              style={{ opacity: locked() ? (isOff(s, f().k) ? OFF_OPACITY : 0.7) : 1 }}
              activeStyle={{ opacity: 0.7 }}
              onPress={() => pick(f())}
            >
              <CheckBox on={on()} />
              <Text class={`text-sm ${locked() ? C.fg2 : C.fg}`}>{choiceLabel(f())}</Text>
            </View>
          );
        }}
      </Each>
      <If when={Boolean(hint())}>
        <Text class={`text-xs ${C.fg3}`}>{hint()}</Text>
      </If>
    </View>
  );
}

function TinBoxes(p: { s: FS; it: Item }) {
  const s = p.s;
  return (
    <View class="flex-row flex-wrap items-center gap-1.5">
      <Each of={p.it.f || []}>
        {(f, j) => {
          const width = j() === 3 ? 76 : 58;
          const locked = () => (rev(), !s.editable || Boolean(f().c) || isLocked(s, f().k));
          return (
            <View class="flex-row items-center gap-1.5">
              <If when={j() > 0}>
                <Text class={`text-sm ${C.fg3}`}>-</Text>
              </If>
              <If when={!locked()} else={<LockedBox text={(rev(), String(s.values[f().k] ?? ""))} width={width} center />}>
                <Box s={s} k={f().k} t="text" width={width} label={f().l} maxLength={f().m} />
              </If>
            </View>
          );
        }}
      </Each>
    </View>
  );
}

function DateButton(p: { s: FS; id: string; date: DateSpec }) {
  return (
    <View
      class={`h-8 px-2 rounded-md items-center justify-center ${(rev(), p.s.picker?.id === p.id ? C.accentSoft : C.inset)}`}
      activeStyle={{ opacity: 0.7 }}
      onPress={() => openPicker(p.s, p.id, p.date)}
    >
      <Text class={`text-xs font-semibold ${C.accent}`}>Pick date</Text>
    </View>
  );
}

/** The calendar card for picker `id` (shown under its item while open). */
function CalendarCard(p: { s: FS; id: string }) {
  const s = p.s;
  const pk = () => (rev(), s.picker && s.picker.id === p.id ? s.picker : null);
  const weeks = () => {
    const c = pk();
    if (!c) return [] as string[][];
    const first = new Date(c.y, c.m - 1, 1);
    const start = new Date(c.y, c.m - 1, 1 - first.getDay());
    const out: string[][] = [];
    for (let w = 0; w < 6; w++) {
      const row: string[] = [];
      for (let d = 0; d < 7; d++) row.push(isoDate(addDays(start, w * 7 + d)));
      out.push(row);
    }
    return out;
  };
  const move = (dm: number) => {
    const c = s.picker;
    if (!c) return;
    const n = new Date(c.y, c.m - 1 + dm, 1);
    c.y = n.getFullYear();
    c.m = n.getMonth() + 1;
    bump();
  };
  const last = new Date().getFullYear() + 10;
  const years = Array.from({ length: last - 1899 }, (_x, i) => last - i);
  const setList = (l: "month" | "year" | null) => {
    if (s.picker) s.picker.list = s.picker.list === l ? null : l;
    bump();
  };
  return (
    <If when={pk()}>
      <View class={`mt-2 p-3 rounded-xl border ${C.line} ${C.card} gap-1 self-start`}>
        <View class="flex-row items-center gap-1 mb-1">
          <Btn label="‹" variant="ghost" onPress={() => move(-1)} />
          <Btn label={`${MONTHS_LONG[(pk()?.m || 1) - 1]} ▾`} onPress={() => setList("month")} />
          <Btn label={`${pk()?.y ?? ""} ▾`} onPress={() => setList("year")} />
          <Btn label="›" variant="ghost" onPress={() => move(1)} />
          <Btn label="Close" variant="ghost" onPress={() => { s.picker = null; bump(); }} />
        </View>
        <If when={pk()?.list === "month"}>
          <View class="flex-row flex-wrap gap-1" style={{ width: 230 }}>
            <Each of={MONTHS_LONG}>
              {(name, i) => <Btn label={name().slice(0, 3)} onPress={() => { if (s.picker) { s.picker.m = i() + 1; s.picker.list = null; } bump(); }} />}
            </Each>
          </View>
        </If>
        <If when={pk()?.list === "year"}>
          <Scroll style={{ maxHeight: 200, width: 230 }}>
            <View class="flex-row flex-wrap gap-1">
              <Each of={years}>
                {(y) => <Btn label={String(y())} onPress={() => { if (s.picker) { s.picker.y = y(); s.picker.list = null; } bump(); }} />}
              </Each>
            </View>
          </Scroll>
        </If>
        <View class="flex-row">
          <Each of={WEEKDAYS}>{(w) => <View style={{ width: 32 }} class="items-center"><Text class={`text-xs ${C.fg3}`}>{w()}</Text></View>}</Each>
        </View>
        <Each of={weeks()}>
          {(week) => (
            <View class="flex-row">
              <Each of={week()}>
                {(day) => {
                  const inMonth = () => Number(day().slice(5, 7)) === pk()?.m;
                  const on = () => day() === pk()?.sel;
                  return (
                    <View
                      class={`items-center justify-center rounded-md ${on() ? C.accentBg : ""}`}
                      style={{ width: 32, height: 32 }}
                      activeStyle={{ opacity: 0.6 }}
                      onPress={() => {
                        const c = s.picker;
                        if (c) pickDate(s, c.date, day());
                      }}
                    >
                      <Text class={`text-sm ${on() ? "text-white dark:text-white" : inMonth() ? C.fg : C.fg3}`}>{String(Number(day().slice(8)))}</Text>
                    </View>
                  );
                }}
              </Each>
            </View>
          )}
        </Each>
      </View>
    </If>
  );
}

/** A schedule (rows x columns); scrolls sideways when wider than the page. */
function ScheduleTable(p: { s: FS; it: Item; id: string }) {
  const s = p.s;
  const it = p.it;
  const colW = 150;
  const numW = 36;
  const cols = it.cols || [];
  const shownCols = cols.map((c, i) => (c.t === "hidden" ? -1 : i)).filter((i) => i >= 0);
  const labels = it.row_labels || null;
  const numbered = !labels || it.numbered !== false;
  const labelW = labels ? 220 : 0;
  const tableW = (numbered ? numW : 0) + labelW + shownCols.length * colW;
  const rows = (it.keys || []).map((keys, r) => ({ keys, r }));
  return (
    <Scroll horizontal style={{ maxWidth: "100%" }} class={`rounded-md border ${C.line}`}>
      <View style={{ width: tableW }}>
        <View class={`flex-row items-end border-b ${C.line} ${C.card}`}>
          <If when={numbered}>
            <View style={{ width: numW }} class="py-2 items-center"><Text class={`text-xs ${C.fg3}`}>#</Text></View>
          </If>
          <If when={Boolean(labels)}>
            <View style={{ width: labelW }} class="py-2 px-2"><Text class={`text-xs font-semibold ${C.fg3}`}>{labels?.caption || ""}</Text></View>
          </If>
          <Each of={shownCols}>
            {(ci) => <View style={{ width: colW }} class="py-2 px-2"><Text class={`text-xs font-semibold ${C.fg3}`}>{cols[ci()].l}</Text></View>}
          </Each>
        </View>
        <Each of={rows}>
          {(row) => {
            const { keys, r } = row();
            const no = numbered ? String((it.start ?? 1) + r) : labels!.rows[r] || String((it.start ?? 1) + r);
            return (
              <View class={`flex-row items-center ${r > 0 ? "border-t " + C.line : ""}`}>
                <If when={numbered}>
                  <View style={{ width: numW }} class="items-center"><Text class={`text-xs ${C.fg3}`}>{no}</Text></View>
                </If>
                <If when={Boolean(labels)}>
                  <View style={{ width: labelW }} class="py-1.5 px-2"><Text class={`text-sm ${C.fg2}`}>{labels?.rows[r] || ""}</Text></View>
                </If>
                <Each of={shownCols}>
                  {(ci) => {
                    const c = ci();
                    const col = cols[c];
                    const k = keys[c];
                    const locked = () => (rev(), !s.editable || Boolean(col.c) || isLocked(s, k));
                    const cellLocked = () => {
                      rev();
                      const value = String(s.values[k] ?? "");
                      const sh = col.t === "money" ? formatMoney(value || (isComputedBox(s, k) ? "0" : ""), col.w) : col.t === "date" ? shownDate(value) : value;
                      const hint = !sh ? reasonFor(s, k) : "";
                      return { text: sh || hint, hint: Boolean(hint), right: !hint && col.t === "money" };
                    };
                    const pickerId = `${p.id}-${r}-${c}`;
                    let cell;
                    if (col.t === "radio" || col.t === "chk") {
                      const on = () => (rev(), String(s.values[k] ?? "") === "true");
                      cell = (
                        <View class="flex-row items-center gap-1" activeStyle={{ opacity: 0.7 }} onPress={() => !locked() && choose(s, { [k]: on() ? "false" : "true" })}>
                          <CheckBox on={on()} locked={locked()} />
                          <If when={locked() && !on() && Boolean(reasonFor(s, k))}>
                            <Text class={`text-xs ${C.fg3}`}>{reasonFor(s, k)}</Text>
                          </If>
                        </View>
                      );
                    } else if (col.t === "sel") {
                      cell = <SelectBox s={s} k={k} label={`${col.l}, row ${no}`} listed={col.o} width={colW - 16} />;
                    } else {
                      cell = (
                        <If when={!locked()} else={<LockedBox text={cellLocked().text} hint={cellLocked().hint} right={cellLocked().right} width={colW - 16} />}>
                          <If when={col.t === "date" && Boolean(it.calendar)} else={<Box s={s} k={k} t={col.t} width={colW - 16} label={`${col.l}, row ${no}`} />}>
                            <View class="gap-1">
                              <Box s={s} k={k} t="date" width={colW - 16} label={`${col.l}, row ${no}`} />
                              <DateButton s={s} id={pickerId} date={{ single: { k, l: col.l, t: "date" } }} />
                              <CalendarCard s={s} id={pickerId} />
                            </View>
                          </If>
                        </If>
                      );
                    }
                    return (
                      <View style={{ width: colW, opacity: (rev(), isOff(s, k) ? OFF_OPACITY : 1) }} class="py-1.5 px-2">
                        {cell}
                      </View>
                    );
                  }}
                </Each>
              </View>
            );
          }}
        </Each>
      </View>
    </Scroll>
  );
}

// =====================================================================
// One item of a part (itemRow in form_page.js)
// =====================================================================

function ItemRow(p: { s: FS; pi: number; ii: number; border: boolean }) {
  const s = p.s;
  const it = s.schema!.parts[p.pi].items[p.ii];
  const id = `${p.pi}:${p.ii}`;
  const fs = it.f || [];
  const allCalc = !it.rows && fs.length > 0 && fs.every((x) => x.c);
  const req = fs.some((x) => x.r);
  const tin = isTinItem(it);
  const od = oneDate(it);
  const radios = fs.filter((f) => f.t === "radio");
  const singles = fs.filter((f) => f.t === "date");
  const split = singles.length || od ? null : dateParts(fs);

  const editKeys = () => (rev(), fs.filter((x) => s.profileEdit.has(x.k)).map((x) => x.k));
  const editingHere = () => editKeys().some((k) => s.editing.has(k));
  const hasEdit = () => editKeys().length > 0 && s.editable;
  const fromProfile = () => (rev(), (tin && !it.typed_tin) || fs.some((x) => s.profile.has(x.k)));
  const penalty = () => (rev(), s.editable ? fs.map((f) => penaltyGroupOf(s, f.k)).find((g) => g && g.applies) || null : null);
  const penaltyOpen = () => {
    const g = penalty();
    return Boolean(g) && (g!.edited || g!.keys.some((k) => s.penaltyOpen.has(k)));
  };
  const needs = () => (rev(), [...new Set(itemKeys(it).map((k) => isOff(s, k)).filter(Boolean))]);
  const msg = () => (rev(), s.dateErrors[id] || s.messages[id] || "");
  const notes = () => (rev(), lockNotesFor(s, it));
  const checkable = () => (rev(), s.editable ? s.checkable[id] || [] : []);
  const fromHint = () => {
    rev();
    const from = it.from;
    if (!from || !s.editable || isLocked(s, from.to)) return "";
    const pesos = wholePesosOf(s.values[from.k]);
    if (!pesos) return "";
    return `${from.l}: ₱${formatMoney(String(pesos.amount), true)}${pesos.rounded ? " (rounded to whole pesos)" : ""}`;
  };
  const pickerOn = (f: Field) => (rev(), s.editable && !f.c && !isLocked(s, f.k));

  let controls;
  if (it.rows) {
    controls = <ScheduleTable s={s} it={it} id={id} />;
  } else if (!fs.length && it.note) {
    controls = <Text class={`text-xs ${C.fg3}`}>{it.note}</Text>;
  } else if (it.l === "Additional details") {
    const open = () => (rev(), s.moreOpen.has(id));
    controls = (
      <View>
        <View class="flex-row items-center gap-1.5 h-7" activeStyle={{ opacity: 0.7 }} onPress={() => { if (s.moreOpen.has(id)) s.moreOpen.delete(id); else s.moreOpen.add(id); bump(); }}>
          <Text class={`text-xs ${C.fg3}`}>{open() ? "▾" : "▸"}</Text>
          <Text class={`text-xs font-semibold ${C.fg2}`}>{`${fs.length} more field${fs.length > 1 ? "s" : ""} not yet placed on the paper layout`}</Text>
        </View>
        <If when={open()}>
          <View class="flex-row flex-wrap mt-2" style={{ columnGap: 16, rowGap: 12, alignItems: "flex-end" }}>
            <Each of={fs}>{(f) => <FieldControl s={s} f={f()} itemLabel={f().l} multi />}</Each>
          </View>
        </If>
      </View>
    );
  } else if (od) {
    const parts = [od.month!, od.day, od.year!].filter(Boolean) as Field[];
    const locked = () => (rev(), !s.editable || parts.every((f) => f.c || isLocked(s, f.k)));
    const lockedText = () => {
      rev();
      const sh = shownDate(oneDateText(s.values, od));
      const hint = !sh ? parts.map((f) => reasonFor(s, f.k)).find(Boolean) || "" : "";
      return { text: sh || hint, hint: Boolean(hint) };
    };
    controls = (
      <View>
        <If when={!locked()} else={<LockedBox text={lockedText().text} hint={lockedText().hint} width={controlWidth("date")} />}>
          <View class="flex-row items-end gap-1">
            <Box s={s} k={od.oneKey!} t="date" width={controlWidth("date")} label={it.l} />
            <DateButton s={s} id={id} date={od} />
          </View>
        </If>
        <CalendarCard s={s} id={id} />
      </View>
    );
  } else if (tin) {
    controls = <TinBoxes s={s} it={it} />;
  } else {
    controls = (
      <View>
        <View class="flex-row flex-wrap" style={{ columnGap: 16, rowGap: 12, alignItems: "flex-end" }}>
          <If when={radios.length > 0}>
            <RadioGroup s={s} radios={radios} />
          </If>
          <Each of={fs.filter((f) => f.t !== "radio")}>
            {(f) => {
              const field = f();
              if (field.t !== "date") return <FieldControl s={s} f={field} itemLabel={it.l} multi={fs.length > 1} />;
              const pid = singles.length === 1 ? id : `${id}-${singles.indexOf(field)}`;
              return (
                <View class="flex-row items-end gap-1">
                  <FieldControl s={s} f={field} itemLabel={it.l} multi={fs.length > 1} />
                  <If when={pickerOn(field)}>
                    <DateButton s={s} id={pid} date={{ single: field }} />
                  </If>
                </View>
              );
            }}
          </Each>
          <If when={Boolean(split) && (rev(), s.editable && !([split?.month, split?.day, split?.year].filter(Boolean) as Field[]).every((f) => f.c || isLocked(s, f.k)))}>
            <DateButton s={s} id={id} date={split || {}} />
          </If>
        </View>
        <CalendarCard s={s} id={id} />
        <Each of={singles.length > 1 ? singles.map((_f, i) => `${id}-${i}`) : []}>{(pid) => <CalendarCard s={s} id={pid()} />}</Each>
      </View>
    );
  }

  return (
    <View
      class={`flex-row gap-3 pt-3.5 pr-5 pb-2 pl-3 ${p.border ? "border-t " + C.line : ""} ${allCalc ? C.card : ""}`}
    >
      <View style={{ width: 44 }}>
        <If when={Boolean(it.no)}>
          <View class={`h-6 px-1.5 rounded-md items-center justify-center self-start ${C.inset}`} style={{ minWidth: 30 }}>
            <Text class={`text-xs font-semibold ${C.fg2}`}>{it.no || ""}</Text>
          </View>
        </If>
      </View>
      <View class="flex-1 gap-1">
        <View class="flex-row items-start gap-2 flex-wrap mb-1.5">
          <Text class={`text-sm font-medium flex-1 ${C.fg}`} style={{ minWidth: 160 }}>{req ? `${it.l} *` : it.l}</Text>
          <If when={Boolean(it.rows)}>
            <Text class={`text-xs ${C.fg3}`}>{`${it.rows} rows`}</Text>
          </If>
          <If when={fromProfile()}>
            <Chip text={editKeys().length ? "From tax profile" : tin ? "From tax profile · change it there" : "From tax profile · repeats a box above"} />
          </If>
          <If when={allCalc}>
            <Chip text="Computed from other items" />
          </If>
          <If when={Boolean(penalty())}>
            <If when={penaltyOpen()} else={<Btn label="Edit penalties" variant="ghost" onPress={() => { const g = penalty(); if (g) openPenalties(s, g); }} />}>
              <Btn label="Use the computed amounts" variant="ghost" onPress={() => { const g = penalty(); if (g) usePenaltyComputed(s, g); }} />
            </If>
          </If>
          <If when={hasEdit()}>
            <If when={!editingHere()} else={<Btn label="Done" variant="ghost" onPress={() => { if (endProfileEdit(s, editKeys())) flush(s); }} />}>
              <Btn label="Edit on this return" variant="ghost" onPress={() => startProfileEdit(s, editKeys())} />
            </If>
          </If>
        </View>
        <Each of={needs()}>{(why) => <Text class={`text-xs mb-1 ${C.fg3}`}>{needsText(why())}</Text>}</Each>
        {controls}
        <If when={Boolean(fromHint())}>
          <View class="flex-row items-center gap-2 mt-1">
            <Text class={`text-xs ${C.fg3}`}>{fromHint()}</Text>
            <Btn label="Use it" variant="ghost" onPress={() => it.from && useFrom(s, it.from)} />
          </View>
        </If>
        <Each of={notes()}>{(n) => <Text class={`text-xs mt-1 ${C.fg3}`}>{n()}</Text>}</Each>
        <View style={{ minHeight: 18 }} class="mt-1">
          <Text class={`text-xs ${C.danger}`}>{msg()}</Text>
        </View>
        <Each of={checkable()} key={(c) => c.token}>
          {(c) => (
            <View class="flex-row mt-1">
              <Btn label={c().label} variant="ghost" onPress={() => markChecked(s, c().token)} />
            </View>
          )}
        </Each>
      </View>
    </View>
  );
}

/** "From tax profile · N filled boxes": a part's profile-filled items, folded. */
function ProfileFold(p: { s: FS; pi: number; border: boolean }) {
  const s = p.s;
  const info = () => {
    rev();
    const tucked = tuckedItems(s, p.pi);
    const items = tucked.map((ii) => s.schema!.parts[p.pi].items[ii]);
    const boxes = (it: Item) => {
      const fs = (it.f || []).filter((f) => s.profile.has(f.k));
      return fs.filter((f) => f.t !== "radio").length + (fs.some((f) => f.t === "radio") ? 1 : 0);
    };
    const filled = items.reduce((n, it) => n + boxes(it), 0);
    const flagged = tucked.filter((ii) => s.messages[`${p.pi}:${ii}`] || s.dateErrors[`${p.pi}:${ii}`]).length;
    return { label: `From tax profile · ${filled} filled box${filled === 1 ? "" : "es"}`, flagged, open: s.profileOpen.has(p.pi) };
  };
  return (
    <View
      class={`flex-row items-center gap-2 pl-3 pr-5 ${C.card} ${p.border ? "border-t " + C.line : ""}`}
      style={{ minHeight: 40 }}
      activeStyle={{ opacity: 0.7 }}
      onPress={() => {
        if (s.profileOpen.has(p.pi)) s.profileOpen.delete(p.pi);
        else s.profileOpen.add(p.pi);
        bump();
      }}
    >
      <Text class={`text-xs ${C.fg3}`}>{info().open ? "▾" : "▸"}</Text>
      <Text class={`text-xs font-semibold ${C.fg2}`}>{info().label}</Text>
      <If when={info().flagged > 0}>
        <Text class={`text-xs ${C.danger}`}>{`· ${info().flagged} need${info().flagged === 1 ? "s" : ""} attention`}</Text>
      </If>
    </View>
  );
}

type Entry = { key: string; kind: "item" | "fold"; ii: number };

function PartCard(p: { s: FS; pi: number }) {
  const s = p.s;
  const part = s.schema!.parts[p.pi];
  const open = () => (rev(), s.openParts.has(p.pi));
  // Rows keep their identity (and their inputs) while the fold opens and closes.
  const memo = new Map<string, Entry>();
  const entries = computed<Entry[]>(() => {
    rev();
    const tucked = tuckedItems(s, p.pi);
    const out: Entry[] = [];
    const add = (e: Entry) => {
      const was = memo.get(e.key);
      if (was) return out.push(was);
      memo.set(e.key, e);
      out.push(e);
    };
    part.items.forEach((_it, ii) => {
      if (tucked.includes(ii)) {
        if (ii !== tucked[0]) return;
        add({ key: "fold", kind: "fold", ii });
        if (s.profileOpen.has(p.pi)) for (const tj of tucked) add({ key: `i${tj}`, kind: "item", ii: tj });
        return;
      }
      add({ key: `i${ii}`, kind: "item", ii });
    });
    return out;
  });
  return (
    <View class={`rounded-2xl border ${C.line} ${C.pane} overflow-hidden`}>
      <View class="flex-row items-center gap-2 pl-3 pr-4" style={{ minHeight: 48 }} activeStyle={{ opacity: 0.7 }} onPress={() => togglePart(s, p.pi)}>
        <Text class={`text-xs ${C.fg3}`}>{open() ? "▾" : "▸"}</Text>
        <Text class={`text-sm font-semibold flex-1 ${C.fg}`}>{partName(part, p.pi)}</Text>
        <Text class={`text-xs ${C.fg3}`}>{`${part.items.length} item${part.items.length > 1 ? "s" : ""}`}</Text>
      </View>
      <If when={open()}>
        <View class={`border-t ${C.line}`}>
          <Each of={entries()}>
            {(e, i) =>
              e().kind === "fold" ? (
                <ProfileFold s={s} pi={p.pi} border={i() > 0} />
              ) : (
                <ItemRow s={s} pi={p.pi} ii={e().ii} border={i() > 0} />
              )
            }
          </Each>
        </View>
      </If>
    </View>
  );
}

// =====================================================================
// The form page (formContent / renderFormPage / renderFormSplit)
// =====================================================================

function FormPage(p: { s: FS }) {
  const s = p.s;
  const ready = () => (rev(), Boolean(s.schema) && !s.loading && !s.error);
  const f = () => (rev(), s.schema);
  const wide = () => rox.viewport().width >= 980;
  const errorsIn = (pi: number) => Object.keys(s.messages).filter((id) => id.startsWith(`${pi}:`)).length;
  const needing = () => (rev(), (s.schema?.parts || []).map((_p, i) => i).filter((i) => errorsIn(i) > 0));
  const openOne = () => (rev(), s.openParts.size === 1 ? [...s.openParts][0] : -1);
  const needsYou = () => {
    rev();
    // A return that has left Draft (sent, confirmed, read-only) can't be changed: recompute-style
    // messages on it are not the user's to fix. Payment shows in the payment panel instead.
    if (!s.schema || !s.editable) return [] as { id: string; pi: number; ii: number; label: string; msg: string }[];
    return Object.entries({ ...s.messages, ...s.dateErrors })
      .filter(([id]) => id !== "form")
      .map(([id, msg]) => {
        const [pi, ii] = id.split(":").map(Number);
        const it = s.schema!.parts[pi]?.items[ii];
        return { id, pi, ii, label: it ? `${it.no ? `Item ${it.no} · ` : ""}${it.l}` : id, msg };
      })
      .sort((a, b) => a.pi - b.pi || a.ii - b.ii);
  };

  const tp = () => picker.data();
  const head = (
    <View class="gap-3">
      <View class="gap-1">
        <Text class={`text-xs ${C.fg2}`}>{f() ? `BIR Form ${f()!.c} · Revision ${f()!.rev}` : `Form ${s.code}`}</Text>
        <Text class={`text-xl font-semibold ${C.fg}`}>{f()?.t || (s.error ? "" : "Opening…")}</Text>
        <If when={Boolean(f())}>
          <View class="flex-row flex-wrap items-center gap-2 mt-1">
            <Chip text={f()?.freq || ""} />
            <Chip text={`${f()?.pages} page${(f()?.pages || 0) > 1 ? "s" : ""}`} />
            <Chip text={`${f()?.n} fields`} />
            <If when={(f()?.q ?? 1) < 0.8}><Chip text="Draft layout" tone="warning" /></If>
            <If when={(rev(), s.status !== "Draft")}><Chip text={(rev(), s.editable ? s.status : `${s.status} · read-only`)} tone={(rev(), s.status === "Queued" ? "accent" : "success")} /></If>
            <If when={(rev(), Boolean(s.amends || s.filedOriginal))}><Chip text="Amended return" tone="warning" /></If>
            <If when={(rev(), Boolean(s.filedOriginalView))}><Chip text="Filed original · read-only" tone="success" /></If>
          </View>
        </If>
      </View>
      <View class={`flex-row flex-wrap items-center gap-3 py-2.5 px-3 rounded-2xl border ${C.line} ${C.card}`}>
        <Text class={`text-sm font-semibold ${C.fg}`}>{tp()?.taxpayer?.name || "Taxpayer"}</Text>
        <Text class={`text-sm ${C.fg2}`}>{formatTin(s.tin.slice(0, 9), s.tin.slice(9) || "00000")}</Text>
        <Text class={`text-xs ${C.fg2}`}>{tp()?.branch?.name || ""}</Text>
        <Text class={`text-xs ${C.fg3}`}>·</Text>
        <Text class={`text-xs ${C.fg2}`}>{s.sample ? "Sample from the Form library" : `For ${periodLabel(f()?.freq || "", s.year, s.period)}`}</Text>
        <If when={(rev(), s.loading)}><Chip text="Opening…" tone="accent" /></If>
        <If when={(rev(), s.checking)}><Chip text="Checking…" tone="accent" /></If>
        <View class="flex-1" />
        <Btn label="View tax profile" variant="ghost" onPress={openTaxProfile} />
      </View>
      <If when={s.sample}><Text class={`text-xs ${C.fg2}`}>{SAMPLE_NOTE}</Text></If>
      <If when={(rev(), Boolean(s.viewOnly) && !s.loading && !s.error)}>
        <Text class={`text-xs ${C.fg2}`}>Opened to view: nothing here can be changed. Open it from the forms list to edit it.</Text>
      </If>
      <SubmitConfirm s={s} />
      <AssistantToggle s={s} />
      <PaymentPanel s={s} />
      <If when={(rev(), s.profileUpdates.length > 0)}><Text class={`text-xs ${C.fg2}`}>{`Updated from your tax profile: ${s.profileUpdates.join("; ")}`}</Text></If>
      <Each of={(rev(), s.notes)}>{(n) => <Text class={`text-xs ${C.fg2}`}>{n().text}</Text>}</Each>
      <If when={(rev(), s.editable && s.profileDiffers.length > 0)}>
        <View class="flex-row flex-wrap items-center gap-2">
          <Text class={`text-xs ${C.fg2}`}>{`Differs from your profile: ${s.profileDiffers.join("; ")}`}</Text>
          <Btn label="Use profile values" variant="ghost" onPress={() => useProfileValues(s)} />
        </View>
      </If>
      <If when={(rev(), Boolean(s.amends || s.filedOriginal))}><Text class={`text-xs ${C.fg2}`}>{`This return amends a filed one. ${FILED_KEPT}`}</Text></If>
      <If when={(rev(), Boolean(s.filedOriginalView))}>
        <Text class={`text-xs ${C.fg2}`}>{(rev(), `This is the version filed${s.filedOriginalView?.filed_on ? ` on ${shortDate(s.filedOriginalView.filed_on)}` : ""}, kept in this return's history; the current return is its amendment. Nothing here can be changed.`)}</Text>
      </If>
      <If when={(rev(), !s.editable && !s.filedOriginalView && Boolean(s.submittedAt || s.confirmedAt))}>
        <Text class={`text-xs ${C.fg2}`}>
          {(rev(), `${[s.submittedAt ? `Sent to BIR ${shortDate(s.submittedAt)}` : "", s.confirmedAt ? `confirmed ${shortDate(s.confirmedAt)}` : ""].filter(Boolean).join(", ")}. Nothing here can be changed.`)}
        </Text>
      </If>
      <If when={(rev(), Boolean(s.printResult))}>
        <View class={`p-3 rounded-xl gap-2 ${C.dangerSoft}`}>
          <Text class={`text-sm font-semibold ${C.fg}`}>{(rev(), s.printResult?.sample ? "The sample's print preview could not be made" : "Print preview could not be made")}</Text>
          <Text class={`text-xs ${C.danger}`} selectable>{(rev(), s.printResult?.error || "")}</Text>
          <View class="flex-row gap-2">
            <Btn label="Try again" onPress={() => print(s)} disabled={(rev(), s.loading || !s.draft)} />
            <Btn label="Dismiss" variant="ghost" onPress={() => { s.printResult = null; bump(); }} />
          </View>
        </View>
      </If>
      <If when={(rev(), Boolean(s.messages.form) || s.formErrors.length > 0)}>
        <Text class={`text-xs ${C.danger}`}>{(rev(), [s.messages.form, ...s.formErrors].filter(Boolean).join(" · "))}</Text>
      </If>
      <If when={(rev(), Boolean(s.error))}>
        <ErrorBox text={(rev(), s.error || "")} onRetry={() => reload(s)} />
      </If>
      <If when={(rev(), s.loading)}>
        <View class="py-10 items-center"><Text class={`text-sm ${C.fg3}`}>{`Opening ${s.code}…`}</Text></View>
      </If>
    </View>
  );

  const needsYouList = () => (
    <If when={needsYou().length > 0}>
      <View class={`rounded-2xl border ${C.line} p-3 gap-1.5`}>
        <View class="flex-row items-center justify-between">
          <Text class={`text-sm font-semibold ${C.danger}`}>{`Needs you · ${needsYou().length}`}</Text>
          <Text class={`text-xs ${C.fg3}`}>Tap one to go to its part</Text>
        </View>
        <Each of={needsYou()} key={(n) => `${n.id}:${n.msg}`}>
          {(n) => (
            <View class="py-1" activeStyle={{ opacity: 0.6 }} onPress={() => jumpTo(s, n().pi)}>
              <Text class={`text-xs font-semibold ${C.fg}`}>{n().label}</Text>
              <Text class={`text-xs ${C.danger}`}>{n().msg}</Text>
            </View>
          )}
        </Each>
      </View>
    </If>
  );

  const badge = (text: () => string, danger: boolean) => (
    <View class={`rounded-full px-1.5 items-center justify-center ${danger ? "bg-[#c62828] dark:bg-[#e05555]" : C.inset}`} style={{ minWidth: 18, height: 18 }}>
      <Text class={`text-xs ${danger ? "text-white dark:text-white" : C.fg2}`}>{text()}</Text>
    </View>
  );

  const parts = () => (
    <View class="gap-4">
      <Each of={s.schema?.parts || []}>{(_p, i) => <PartCard s={s} pi={i()} />}</Each>
    </View>
  );

  const attention = () => (
    <If when={needing().length > 0}>
      <View class="px-2 pb-1.5" activeStyle={{ opacity: 0.6 }} onPress={() => jumpTo(s, needing().find((i) => i > openOne()) ?? needing()[0])}>
        <Text class={`text-xs ${C.danger}`}>{`${needing().length} part${needing().length > 1 ? "s" : ""} need${needing().length > 1 ? "" : "s"} attention · next ↓`}</Text>
      </View>
    </If>
  );

  const sideNav = () => (
    <Scroll style={{ width: 236 }}><View class="pt-5 pb-10 pl-4 pr-2 gap-0.5">
      <View class="flex-row items-center px-2 pb-1.5">
        <Text class={`text-xs font-semibold flex-1 ${C.fg3}`}>On this form</Text>
        <Text class={`text-xs ${C.fg3}`}>{openOne() >= 0 ? `${openOne() + 1} of ${s.schema?.parts.length || 0}` : `${s.schema?.parts.length || 0} parts`}</Text>
      </View>
      {attention()}
      <Each of={s.schema?.parts || []}>
        {(part, i) => {
          const current = () => (rev(), s.openParts.has(i()) && s.openParts.size === 1);
          return (
            <View class={`flex-row items-start gap-2 px-2 py-1.5 rounded-md ${current() ? C.accentSoft : ""}`} activeStyle={{ opacity: 0.6 }} onPress={() => jumpTo(s, i())}>
              <Text class={`text-sm flex-1 ${current() ? C.fg : C.fg2}`}>{partName(part(), i())}</Text>
              <View class="flex-row gap-1">
                <If when={(rev(), errorsIn(i()) > 0)}>{badge(() => (rev(), String(errorsIn(i()))), true)}</If>
                {badge(() => String(part().items.length), false)}
              </View>
            </View>
          );
        }}
      </Each>
    </View></Scroll>
  );

  const chipNav = () => (
    <View class="flex-row flex-wrap gap-1">
      <Each of={s.schema?.parts || []}>
        {(part, i) => (
          <View class="flex-row items-center gap-1" activeStyle={{ opacity: 0.6 }} onPress={() => jumpTo(s, i())}>
            <Chip text={`${partName(part(), i())} · ${part().items.length}`} tone={(rev(), s.openParts.has(i()) ? "accent" : "neutral")} />
            <If when={(rev(), errorsIn(i()) > 0)}>{badge(() => (rev(), String(errorsIn(i()))), true)}</If>
          </View>
        )}
      </Each>
    </View>
  );

  return (
    <View style={{ flex: 1 }}>
      <FormBar s={s} />
      <If
        when={ready() && wide()}
        else={
          <Scroll style={{ flex: 1 }} ref={(n) => (formScroll = n)}><View class="p-5 gap-4 pb-16">
            {head}
            <If when={ready()}>
              {needsYouList()}
              {attention()}
              {chipNav()}
              {parts()}
            </If>
          </View></Scroll>
        }
      >
        <View class="flex-row" style={{ flex: 1 }}>
          {sideNav()}
          <Scroll style={{ flex: 1 }} ref={(n) => (formScroll = n)}><View class="py-5 pl-5 pr-6 gap-4 pb-16">
            {head}
            {needsYouList()}
            {parts()}
          </View></Scroll>
        </View>
      </If>
    </View>
  );
}

/** Rox's `submit`: the return's Validate result and, when it passes, the user's own Submit. */
function SubmitConfirm(p: { s: FS }) {
  const s = p.s;
  const close = () => {
    s.submitAsk = false;
    bump();
  };
  const shown = () => (rev(), Boolean(s.submitAsk) && s.editable && !s.sample && !s.loading && !s.error && s.status === "Draft");
  const problems = () => (rev(), s.errors.length + s.formErrors.length);
  return (
    <If when={shown()}>
      <View class={`p-3 rounded-xl gap-2 border ${C.line} ${(rev(), problems() ? C.dangerSoft : C.accentSoft)}`}>
        <Text class={`text-sm font-semibold ${C.fg}`}>
          {(rev(), s.checking ? `Checking ${s.code}…` : problems() ? `${problems()} item${problems() > 1 ? "s need" : " needs"} attention before you submit` : `${s.code} is ready to submit`)}
        </Text>
        <Text class={`text-xs ${C.fg2}`}>
          {(rev(), problems() ? "Fix the items listed under Needs you, then check again." : "Submitting queues this return; this computer sends it to BIR in the background. Review every box first.")}
        </Text>
        <View class="flex-row gap-2">
          <If when={(rev(), !s.checking && problems() === 0)} else={<Btn label={(rev(), s.checking ? "Checking…" : "Check again")} onPress={() => check(s)} disabled={(rev(), s.checking)} />}>
            <Btn label="Submit" variant="primary" small={false} onPress={() => { close(); queue(s); }} />
          </If>
          <Btn label="Not now" variant="ghost" onPress={close} />
        </View>
      </View>
    </If>
  );
}

/** "Let the assistant submit <form> for this taxpayer": the standing OK, granted or revoked here. */
function AssistantToggle(p: { s: FS }) {
  const s = p.s;
  const shown = () => (rev(), !s.sample && !s.filedOriginalView && s.assistantMay !== null && digits(s.tin).length >= 9);
  const on = () => (rev(), s.assistantMay === true);
  return (
    <If when={shown()}>
      <View class="flex-row items-center gap-2 self-start" activeStyle={{ opacity: 0.6 }} onPress={() => setAssistantMay(s, !on())}>
        <CheckBox on={on()} />
        <Text class={`text-xs ${C.fg2}`}>{`Let the assistant submit ${s.code} for this taxpayer`}</Text>
      </View>
    </If>
  );
}

/** Payment, apart from BIR's filing confirmation: what is recorded, and Mark as paid (with its details) /
 *  Attach receipt. Shown on every return that has left Draft, even when the payments could not be read. */
function PaymentPanel(p: { s: FS }) {
  const s = p.s;
  const shown = () => (rev(), sentOrQueued(s) && !s.loading && !s.error);
  const paid = () => (rev(), (s.payments || []).length > 0);
  const hasReceipt = () => (rev(), (s.payments || []).some((x) => x.proof.kind === "file"));
  const form = () => (rev(), s.payForm || null);
  const field = (key: "paid_at" | "amount" | "channel" | "reference", label: string, placeholder: string, width: number) => (
    <View class="gap-0.5">
      <Text class={`text-xs ${C.fg2}`}>{label}</Text>
      <Input
        class={C.input}
        style={{ width }}
        placeholder={placeholder}
        value={(rev(), form()?.[key] || "")}
        onChange={(e) => {
          if (s.payForm) s.payForm[key] = typed(e);
        }}
        autoCapitalize="none"
        autoCorrect={false}
      />
    </View>
  );
  return (
    <If when={shown()}>
      <View class={`gap-2 py-2 px-3 rounded-xl border ${C.line}`}>
        <View class="flex-row flex-wrap items-center gap-2">
          <Chip text={(rev(), paid() ? "Paid" : "Payment not recorded")} tone={(rev(), paid() ? "success" : "warning")} />
          <View class="flex-1 gap-0.5" style={{ minWidth: 200 }}>
            <If when={paid()} else={<Text class={`text-xs ${C.fg2}`}>{(rev(), s.paymentsError || "No payment recorded yet. The BIR payments routine records payment emails; you can also mark it paid here.")}</Text>}>
              <Each of={(rev(), s.payments || [])} key={(x) => x.id}>
                {(x) => (
                  <View class="flex-row flex-wrap items-center gap-2">
                    <Text class={`text-xs ${C.fg2}`}>{paymentLine(x())}</Text>
                    <If when={(rev(), s.removeAsk === x().id)} else={<Btn label="Remove" variant="ghost" onPress={() => { s.removeAsk = x().id; bump(); }} disabled={(rev(), s.paying)} />}>
                      <Text class={`text-xs ${C.fg}`}>Remove this payment?</Text>
                      <Btn label="Remove" variant="danger" onPress={() => removePayment(s, x().id)} disabled={(rev(), s.paying)} />
                      <Btn label="Cancel" variant="ghost" onPress={() => { s.removeAsk = null; bump(); }} />
                    </If>
                  </View>
                )}
              </Each>
            </If>
          </View>
          <If when={!paid() && !form()}>
            <Btn label="Mark as paid" onPress={() => { s.payForm = { paid_at: isoToday(), amount: "", channel: "", reference: "" }; bump(); }} disabled={(rev(), s.paying)} />
          </If>
          <If when={paid() && Boolean(rox.preview)}>
            <Btn label="View receipt" variant="ghost" onPress={() => viewReceipt(s)} />
          </If>
          <If when={(rev(), Boolean(gmailOf(s)))}>
            <Btn label="Open in Gmail" variant="ghost" onPress={() => rox.open({ url: gmailOf(s) }).catch((e) => toast(errText(e), "danger"))} />
          </If>
          <Btn label={(rev(), hasReceipt() ? "Replace receipt" : "Attach receipt")} variant="ghost" onPress={() => attachReceipt(s)} disabled={(rev(), s.paying)} />
        </View>
        <If when={Boolean(form())}>
          <View class="gap-2">
            <View class="flex-row flex-wrap gap-3">
              {field("paid_at", "Paid on", "2026-10-26", 130)}
              {field("amount", "Amount (PHP)", "1,629.94", 130)}
              {field("channel", "Paid through", "MyEG, GCash, a bank…", 180)}
              {field("reference", "Reference (optional)", "Reference no.", 180)}
            </View>
            <Text class={`text-xs ${C.fg3}`}>Leave the amount and channel empty to mark it paid without details.</Text>
            <View class="flex-row gap-2">
              <Btn label={(rev(), s.paying ? "Saving…" : "Save payment")} variant="primary" onPress={() => markPaid(s)} disabled={(rev(), s.paying)} />
              <Btn label="Cancel" variant="ghost" onPress={() => { s.payForm = null; bump(); }} />
            </View>
          </View>
        </If>
      </View>
    </If>
  );
}

/** The top bar: back to the forms, the form's code, and its actions (the head's actions in form_page.js). */
function FormBar(p: { s: FS }) {
  const s = p.s;
  const busy = () => (rev(), s.loading || !s.draft);
  const leave = () => {
    s.picker = null;
    view.set("picker");
    picker.refresh();
  };
  return (
    <View class={`flex-row flex-wrap items-center gap-2 px-4 py-2.5 border-b ${C.line} ${C.pane}`}>
      <Btn label="‹ Forms" variant="ghost" onPress={leave} />
      <Text class={`text-sm font-semibold flex-1 ${C.fg}`} numberOfLines={1}>
        {(rev(), `${s.code}${s.dirty ? " · unsaved edits" : ""}`)}
      </Text>
      <Btn label="Ask the assistant" variant="ghost" onPress={() => askAssistant(s)} disabled={busy() || Boolean(s.sample)} />
      <Btn label="Print preview" onPress={() => print(s)} disabled={busy()} />
      <If when={(rev(), Boolean(s.filedOriginal) && !s.filedOriginalView)}>
        <Btn label="View the filed original" onPress={() => openFiledOriginal(s)} />
      </If>
      <If when={(rev(), Boolean(s.filedOriginalView))}>
        <Btn label="Back to the amendment" onPress={() => s.back && openForm(s.back)} />
      </If>
      <If when={(rev(), !s.filedOriginalView && Boolean(s.sample))}>
        <If when={(rev(), s.editable)}><Btn label={(rev(), s.checking ? "Checking…" : "Check")} onPress={() => check(s)} disabled={busy()} /></If>
        <Btn label="Queue" onPress={() => queue(s)} disabled={busy()} />
        <Btn label="Save" variant="primary" onPress={() => saveSample(s)} disabled={busy()} />
      </If>
      <If when={(rev(), !s.filedOriginalView && !s.sample && s.status === "Queued")}>
        <Btn label="Cancel queue" onPress={() => cancelQueue(s)} />
      </If>
      <If when={(rev(), !s.filedOriginalView && !s.sample && s.status !== "Queued" && s.editable)}>
        <Btn label={(rev(), s.checking ? "Checking…" : "Check")} onPress={() => check(s)} disabled={busy()} />
        <Btn label="Queue" onPress={() => queue(s)} disabled={busy()} />
        <Btn label="Save draft" variant="primary" onPress={() => save(s)} disabled={busy()} />
      </If>
    </View>
  );
}

// =====================================================================
// Overlays: stored-return offer, sample period, removal question; toast
// =====================================================================

function Modal(p: { children?: any; onDismiss: () => void }) {
  return (
    <View style={{ position: "absolute", top: 0, left: 0, right: 0, bottom: 0, alignItems: "center", justifyContent: "center" }}>
      <View style={{ position: "absolute", top: 0, left: 0, right: 0, bottom: 0 }} class="bg-black/40" onPress={p.onDismiss} />
      <View class={`rounded-2xl p-5 gap-3 border ${C.line} ${C.pane}`} style={{ width: 420, maxWidth: "92%" }}>
        {p.children}
      </View>
    </View>
  );
}

function RemovalModal() {
  const held = cur()?.removal;
  if (!held) return null;
  return (
    <Modal onDismiss={cancelRemoval}>
      <Text class={`text-base font-semibold ${C.fg}`}>{held.r.removes.message}</Text>
      <Btn label="Continue" variant="primary" small={false} onPress={confirmRemoval} />
      <Btn label="Cancel" small={false} onPress={cancelRemoval} />
    </Modal>
  );
}

function ExistingModal(p: { existing: any }) {
  const offer = existingOfferFor(p.existing);
  const fast = signal(fastLaneDrafts);
  return (
    <Modal onDismiss={() => takeExisting("keep")}>
      <View class="gap-0.5">
        <Text class={`text-base font-semibold ${C.fg}`}>{offer.title}</Text>
        <If when={Boolean(offer.subtitle)}><Text class={`text-xs ${C.fg2}`}>{offer.subtitle}</Text></If>
      </View>
      <Each of={offer.choices}>
        {(c) => (
          <View class="gap-1">
            <Btn label={c().label} variant={c().variant as any} small={false} onPress={() => takeExisting(c().id)} />
            <Text class={`text-xs ${C.fg2}`}>{c().hint}</Text>
          </View>
        )}
      </Each>
      <If when={offer.fastLane}>
        <View class="flex-row items-center gap-2" activeStyle={{ opacity: 0.7 }} onPress={() => { fastLaneDrafts = !fastLaneDrafts; fast.set(fastLaneDrafts); }}>
          <CheckBox on={fast()} />
          <Text class={`text-sm ${C.fg}`}>Don't ask again for drafts (this session)</Text>
        </View>
      </If>
    </Modal>
  );
}

function SamplePeriodModal(p: { ask: SamplePeriodAsk }) {
  const ask = p.ask;
  const quarter = signal(ask.quarter);
  const parts = [...ask.state.periodParts.values()];
  const quarters = parts.filter((x) => x.part === "quarter");
  return (
    <Modal onDismiss={() => overlay.set(null)}>
      <View class="gap-0.5">
        <Text class={`text-base font-semibold ${C.fg}`}>Which period is this return for?</Text>
        <Text class={`text-xs ${C.fg2}`}>Save makes this sample your return for that period.</Text>
      </View>
      <Each of={parts.filter((x) => x.part !== "quarter")}>
        {(pp) => (
          <View class="gap-1">
            <Text class={`text-xs ${C.fg2}`}>{PERIOD_PART_LABELS[pp().part] || pp().part}</Text>
            <Input class={C.input} defaultValue={ask.values[pp().key] || ""} keyboardType="numbers-and-punctuation" onChange={(e) => (ask.values[pp().key] = typed(e))} />
          </View>
        )}
      </Each>
      <If when={quarters.length > 0}>
        <View class="gap-1">
          <Text class={`text-xs ${C.fg2}`}>{PERIOD_PART_LABELS.quarter}</Text>
          <View class="flex-row flex-wrap gap-2">
            <Each of={quarters}>
              {(q) => <Btn label={`Q${q().quarter}`} variant={quarter() === q().key ? "primary" : "secondary"} onPress={() => { ask.quarter = q().key; quarter.set(q().key); }} />}
            </Each>
          </View>
        </View>
      </If>
      <If when={Boolean(ask.error)}><Text class={`text-xs ${C.danger}`}>{ask.error}</Text></If>
      <View class="flex-row justify-end gap-2">
        <Btn label="Cancel" variant="ghost" onPress={() => overlay.set(null)} />
        <Btn label="Save" variant="primary" onPress={() => takeSamplePeriod(ask)} />
      </View>
    </Modal>
  );
}

function Overlays() {
  return (
    <>
      {() => {
        const ov = overlay();
        return untracked(() => {
          if (!ov) return null;
          if (ov.kind === "removal") return <RemovalModal />;
          if (ov.kind === "existing") return <ExistingModal existing={ov.existing} />;
          return <SamplePeriodModal ask={ov.ask} />;
        });
      }}
    </>
  );
}

function ToastView() {
  return (
    <If when={toastSig()}>
      {(t) => (
        <View style={{ position: "absolute", left: 0, right: 0, bottom: 20, alignItems: "center" }} pointerEvents="box-none">
          <View class={`flex-row items-center gap-3 px-4 py-2.5 rounded-xl border ${C.line} ${C.card}`} style={{ maxWidth: 560 }}>
            {/* the box fits its message: flex-1 would start the text at zero width and show only × */}
            <Text class={`text-sm ${t().tone === "danger" ? C.danger : C.fg}`} style={{ flexShrink: 1 }}>{t().text}</Text>
            <If when={Boolean(t().undo)}>
              <Btn label="Undo" variant="ghost" onPress={() => { const u = t().undo; toastSig.set(null); u?.(); }} />
            </If>
            <Btn label="×" variant="ghost" onPress={() => toastSig.set(null)} />
          </View>
        </View>
      )}
    </If>
  );
}

// =====================================================================
// The form picker for the focused taxpayer (taxpayer.js Forms tab,
// home.js due / as-needed rows, the start-a-return dialog, Filings,
// and the Form library)
// =====================================================================

type PickerData = { taxpayer: any | null; branch: any | null; index: IndexEntry[]; years: number[] };
type YearData = { year: number; forms: string[]; templates: any[]; deadlines: any[]; asNeeded: any[] };

const picker = (() => {
  const data = signal<PickerData | null>(null);
  const error = signal("");
  const busy = signal(false);
  const year = signal<number | null>(null);
  const yd = signal<YearData | null>(null);
  const ydError = signal("");
  const soon = signal<any[]>([]);
  const returns = signal<any[]>([]);
  const listsError = signal("");
  /** The form row shown open (a deep link with only `form_code` opens on it). */
  const sel = signal<string | null>(null);
  let loadedOnce = false;
  /** A year the deep link asked for, used once the years are known. */
  let wantYear: number | null = null;

  async function loadYear(y: number) {
    ydError.set("");
    try {
      const tin = TIN();
      const [forms, templates, deadlines, asNeeded] = await Promise.all([
        H.yearlyForms(tin, y),
        H.templates(tin),
        H.deadlines(`${y}-01-01`, `${y + 1}-12-31`),
        H.asNeeded(y),
      ]);
      if (year() !== y) return;
      yd.set({
        year: y, forms, templates,
        deadlines: deadlines.filter((d) => d.tin === tin && d.taxable_year === y),
        asNeeded: asNeeded.filter((f) => f.tin === tin),
      });
    } catch (e) {
      ydError.set(errText(e));
    }
  }

  async function loadLists() {
    listsError.set("");
    try {
      const tin = TIN();
      const today = new Date();
      const [near, rows] = await Promise.all([H.deadlines(isoDate(addDays(today, -30)), isoDate(addDays(today, 30))), H.returns("all")]);
      soon.set(near.filter((d) => d.tin === tin && (daysUntil(d.due_date) >= 0 || d.status === "Not started" || d.status === "Draft")));
      returns.set(rows.filter((r) => r.tin === tin));
    } catch (e) {
      listsError.set(errText(e));
    }
  }

  async function load() {
    busy.set(true);
    error.set("");
    try {
      const tin = TIN();
      const [tps, index, years] = await Promise.all([H.taxpayers(), H.index(), H.taxYears(tin)]);
      const taxpayer = tps.find((t) => t.tin_root === ROOT()) || null;
      const branch = taxpayer?.branches?.find((b: any) => b.tin === tin) || null;
      const ys = [...years].sort((a, b) => a - b);
      data.set({ taxpayer, branch, index, years: ys });
      const now = new Date().getFullYear();
      let y = year();
      if (wantYear != null && ys.includes(wantYear)) y = wantYear;
      wantYear = null;
      if (y == null || !ys.includes(y)) y = ys.includes(now) ? now : ys.length ? ys[ys.length - 1] : null;
      year.set(y);
      loadedOnce = true;
      await Promise.all([y != null ? loadYear(y) : Promise.resolve(), loadLists()]);
    } catch (e) {
      error.set(errText(e));
    } finally {
      busy.set(false);
    }
  }

  function selectYear(y: number) {
    year.set(y);
    yd.set(null);
    loadYear(y);
  }

  /** Statuses change after a save or queue: read the lists again (quietly). */
  function refresh() {
    if (!loadedOnce) return load();
    const y = year();
    if (y != null) loadYear(y);
    loadLists();
  }

  function focus(code: string, y: number | null) {
    sel.set(code);
    wantYear = y;
  }

  return { data, error, busy, year, yd, ydError, soon, returns, listsError, sel, load, selectYear, refresh, focus };
})();

function FormsPicker() {
  const tab = signal<"forms" | "returns" | "library">("forms");
  const sel = picker.sel;
  const libQ = signal("");
  const libFreq = signal("All");
  const libScope = signal<"owned" | "all">("owned");
  picker.load();

  const yearForms = () => {
    const y = picker.yd();
    const idx = picker.data()?.index || [];
    if (!y) return [] as IndexEntry[];
    const codes = new Set(y.forms.map(normCode));
    return idx.filter((f) => codes.has(f.c));
  };
  const schedule = (code: string) => {
    const rows = (picker.yd()?.deadlines || []).filter((d) => d.form_code === code);
    const open = rows.filter((d) => d.status === "Not started" || d.status === "Draft");
    const upcoming = open.find((d) => daysUntil(d.due_date) >= 0) || open[open.length - 1];
    if (!rows.length) return { sub: `Filed in ${picker.year()}`, due: "" };
    if (!upcoming) return { sub: "Every period filed", due: "" };
    return { sub: `Next: ${upcoming.period_label}`, due: `Due ${shortDate(upcoming.due_date)}` };
  };
  const tin = TIN();
  const openReturn = (code: string, year: number, period: number, key?: number | null) =>
    openForm({ tin, code, year, period, ...(key != null ? { returnKey: key } : {}) });

  const tabs: { id: "forms" | "returns" | "library"; label: string }[] = [
    { id: "forms", label: "Forms" },
    { id: "returns", label: "Returns" },
    { id: "library", label: "Form library" },
  ];

  const dueRow = (d: any) => {
    const n = daysUntil(d.due_date);
    const [, m, day] = String(d.due_date).split("-").map(Number);
    const open = d.status === "Not started" || d.status === "Draft";
    const when = open && n < 0 ? `Overdue ${-n} day${-n === 1 ? "" : "s"}` : n === 0 ? "Today" : n < 0 ? `${-n} days ago` : `In ${n} days`;
    return (
      <View class={`flex-row items-center gap-3 px-3 py-2.5 border-t ${C.line}`} activeStyle={{ opacity: 0.6 }} onPress={() => openReturn(d.form_code, d.taxable_year, d.period, d.return_key)}>
        <View class="items-center" style={{ width: 44 }}>
          <Text class={`text-xs font-semibold ${C.fg3}`}>{(MONTHS[m - 1] || "").toUpperCase()}</Text>
          <Text class={`text-lg font-semibold ${C.fg}`}>{String(day)}</Text>
        </View>
        <View class="flex-1">
          <Text class={`text-sm font-semibold ${C.fg}`}>{`${d.form_code} · ${d.period_label}`}</Text>
          <Text class={`text-xs ${open && n < 0 ? C.danger : C.fg2}`}>{when}</Text>
        </View>
        <Chip text={d.status} tone={open && n < 0 ? "danger" : statusTone(d.status)} />
      </View>
    );
  };

  const formRow = (f: IndexEntry) => {
    const opened = () => sel() === f.c;
    const tpl = () => (picker.yd()?.templates || []).some((t) => t.form_code === f.c);
    const periods = () => {
      const y = picker.year() || new Date().getFullYear();
      const rows = (picker.yd()?.deadlines || []).filter((d) => d.form_code === f.c);
      return periodOptions(f.freq, y).map((o) => {
        const d = rows.find((x) => x.period === o.value);
        return { ...o, status: d?.status || "Not started", key: d?.return_key ?? null, due: d?.due_date || "" };
      });
    };
    return (
      <View class={`border-t ${C.line}`}>
        <View class="flex-row items-center gap-3 px-3 py-2.5" activeStyle={{ opacity: 0.6 }} onPress={() => sel.set(opened() ? null : f.c)}>
          <Text class={`text-sm font-semibold ${C.fg}`} style={{ width: 64 }}>{f.c}</Text>
          <View class="flex-1">
            <Text class={`text-sm ${C.fg}`}>{f.t}</Text>
            <Text class={`text-xs ${C.fg2}`}>{`${schedule(f.c).sub}${schedule(f.c).due ? ` · ${schedule(f.c).due}` : ""}`}</Text>
          </View>
          <Chip text={f.freq} />
          <Chip text={tpl() ? "Template" : "No template"} tone={tpl() ? "success" : "neutral"} />
          <Text class={`text-xs ${C.fg3}`}>{opened() ? "▾" : "▸"}</Text>
        </View>
        <If when={opened()}>
          <View class="px-3 pb-3 gap-2" style={{ paddingLeft: 88 }}>
            <Text class={`text-xs ${C.fg2}`}>Choose the period to open:</Text>
            <View class="flex-row flex-wrap gap-2">
              <Each of={periods()} key={(o) => o.value}>
                {(o) => (
                  <View class={`px-3 py-2 rounded-lg border ${C.line} ${C.card} gap-0.5`} activeStyle={{ opacity: 0.6 }} onPress={() => openReturn(f.c, picker.year() || new Date().getFullYear(), o().value, o().key)}>
                    <Text class={`text-sm font-semibold ${C.fg}`}>{o().label}</Text>
                    <Text class={`text-xs ${o().status === "Not started" ? C.fg3 : C.fg2}`}>{`${o().status}${o().due ? ` · due ${shortDate(o().due)}` : ""}`}</Text>
                  </View>
                )}
              </Each>
            </View>
          </View>
        </If>
      </View>
    );
  };

  const asNeededRows = (f: any) => (
    <View class={`border-t ${C.line}`}>
      <View class="flex-row items-center gap-3 px-3 py-2.5">
        <Text class={`text-sm font-semibold ${C.fg}`} style={{ width: 64 }}>{f.form_code}</Text>
        <View class="flex-1">
          <Text class={`text-sm ${C.fg}`}>{`${f.form_code} · ${f.form_name}`}</Text>
          <Text class={`text-xs ${C.fg2}`}>{f.due_rule}</Text>
        </View>
        <Btn label="+ Start a return" onPress={() => openReturn(f.form_code, f.taxable_year, 1)} />
      </View>
      <Each of={(f.undated || []) as any[]}>
        {(r) => (
          <View class="flex-row items-center gap-3 px-3 py-2" style={{ paddingLeft: 88 }}>
            <View class="flex-1">
              <Text class={`text-sm ${C.fg}`}>{f.trigger ? `No ${String(f.trigger).toLowerCase()} date yet` : "Saved return"}</Text>
              <Text class={`text-xs ${C.fg2}`}>{`Saved ${shortDate(r().updated_at)}`}</Text>
            </View>
            <Chip text={r().status} tone={statusTone(r().status)} />
            <Btn label="Open" variant="ghost" onPress={() => openReturn(f.form_code, f.taxable_year, r().period, r().return_key)} />
          </View>
        )}
      </Each>
    </View>
  );

  const returnRow = (r: any) => (
    <View class={`flex-row items-center gap-3 px-3 py-2.5 border-t ${C.line}`}>
      <Text class={`text-sm font-semibold ${C.fg}`} style={{ width: 64 }}>{r.form_code}</Text>
      <View class="flex-1">
        <Text class={`text-sm ${C.fg}`}>{`${r.form_code} · ${r.period_label}`}</Text>
        <Text class={`text-xs ${C.fg2}`}>{`Updated ${shortDate(r.updated_at)}`}</Text>
      </View>
      <If when={r.status === "Queued"}>
        <Btn label="Cancel queue" variant="ghost" onPress={async () => {
          try {
            await H.cancelQueued(r.tin, r.form_code, r.taxable_year, r.period || null);
            toast("Queue cancelled. The return is an editable draft again.");
            picker.refresh();
          } catch (e) {
            toast(errText(e), "danger");
          }
        }} />
      </If>
      <Chip text={r.status} tone={statusTone(r.status)} />
      <Btn label="Open" variant="ghost" onPress={() => openForm({ tin: r.tin, code: r.form_code, year: r.taxable_year, period: r.period, ...(r.return_key != null ? { returnKey: r.return_key } : {}) })} />
    </View>
  );

  const section = (title: () => string, children: any) => (
    <View class="gap-2">
      <Text class={`text-xs font-semibold ${C.fg3}`}>{title().toUpperCase()}</Text>
      <View class={`rounded-2xl border ${C.line} overflow-hidden`}>{children}</View>
    </View>
  );

  const library = () => {
    const idx = picker.data()?.index || [];
    const owned = new Set((picker.yd()?.forms || []).map(normCode));
    const q = libQ().trim().toLowerCase();
    return idx.filter((f) => {
      const vat = /Value-Added/i.test(f.t) ? " vat" : "";
      const text = `${f.c} ${f.c.replace(/-/g, "")} ${f.t}${vat}`.toLowerCase();
      return (libScope() === "all" || owned.has(f.c)) && (libFreq() === "All" || f.freq === libFreq()) && (!q || text.includes(q));
    });
  };

  return (
    <View style={{ flex: 1 }}>
      <View class={`px-5 pt-4 pb-3 gap-2 border-b ${C.line}`}>
        <View class="flex-row flex-wrap items-center gap-2">
          <Text class={`text-xl font-semibold ${C.fg}`}>{picker.data()?.taxpayer?.name || "BIR Forms"}</Text>
          <Text class={`text-sm ${C.fg2}`}>{formatTin(ROOT(), TIN().slice(9) || "00000")}</Text>
          <Text class={`text-sm ${C.fg2}`}>{picker.data()?.branch?.name || ""}</Text>
          <View class="flex-1" />
          <Btn label="View tax profile" variant="ghost" onPress={openTaxProfile} />
        </View>
        <View class="flex-row flex-wrap items-center gap-2">
          <Each of={tabs}>
            {(t) => (
              <View class={`px-3 py-1.5 rounded-full ${tab() === t().id ? C.accentBg : C.inset}`} activeStyle={{ opacity: 0.7 }} onPress={() => tab.set(t().id)}>
                <Text class={`text-sm font-semibold ${tab() === t().id ? "text-white dark:text-white" : C.fg}`}>{t().label}</Text>
              </View>
            )}
          </Each>
          <View class="flex-1" />
          <If when={(rev(), Boolean(cur()) && !cur()!.loading && !cur()!.error)}>
            <Btn label={(rev(), `Back to ${cur()?.code}${cur()?.dirty ? " (unsaved edits)" : ""}`)} variant="primary" onPress={() => view.set("form")} />
          </If>
          <Btn label={picker.busy() ? "Loading…" : "Refresh"} variant="ghost" onPress={() => picker.load()} />
        </View>
      </View>
      <Scroll style={{ flex: 1 }}><View class="p-5 gap-5 pb-16">
        <If when={Boolean(picker.error())}>
          <ErrorBox text={`Couldn't load this taxpayer: ${picker.error()}`} onRetry={() => picker.load()} />
        </If>
        <If when={!picker.data() && !picker.error()}>
          <Text class={`text-sm ${C.fg3}`}>Loading…</Text>
        </If>
        <If when={Boolean(picker.data())}>
          <If when={Boolean(picker.data() && !picker.data()!.taxpayer)}>
            <ErrorBox text="This taxpayer is not in this database (or is outside this app's scope)." onRetry={() => picker.load()} />
          </If>
          <If when={tab() === "forms"}>
            <If when={(picker.data()?.years.length || 0) > 0} else={
              <View class="gap-2">
                <Text class={`text-sm ${C.fg2}`}>{`${picker.data()?.branch?.name || "This branch"} has no tax year yet. Add one in the tax profile to choose the forms it files.`}</Text>
                <View class="flex-row"><Btn label="Open the tax profile" onPress={openTaxProfile} /></View>
              </View>
            }>
              <View class="flex-row flex-wrap items-center gap-2">
                <Text class={`text-xs ${C.fg2}`}>Tax year</Text>
                <Each of={picker.data()?.years || []}>
                  {(y) => (
                    <View class={`px-3 py-1 rounded-md ${picker.year() === y() ? C.accentSoft : C.inset}`} activeStyle={{ opacity: 0.7 }} onPress={() => picker.selectYear(y())}>
                      <Text class={`text-sm font-semibold ${picker.year() === y() ? C.accent : C.fg}`}>{String(y())}</Text>
                    </View>
                  )}
                </Each>
              </View>
              <If when={Boolean(picker.ydError())}>
                <ErrorBox text={`Couldn't load ${picker.year()}: ${picker.ydError()}`} onRetry={() => picker.selectYear(picker.year()!)} />
              </If>
              <If when={Boolean(picker.listsError())}>
                <ErrorBox text={`Couldn't load deadlines and returns: ${picker.listsError()}`} onRetry={() => picker.refresh()} />
              </If>
              <If when={picker.soon().length > 0}>
                {section(() => "Due in the next 30 days", <Each of={picker.soon()}>{(d) => dueRow(d())}</Each>)}
              </If>
              {section(
                () => `Forms filed in ${picker.year() ?? ""}`,
                <If when={Boolean(picker.yd())} else={<View class="p-3"><Text class={`text-sm ${C.fg3}`}>Loading…</Text></View>}>
                  <If when={yearForms().length > 0} else={<View class="p-3"><Text class={`text-sm ${C.fg2}`}>{`No forms for ${picker.year()} yet. Choose them in the tax profile, or open a sample from the Form library.`}</Text></View>}>
                    <Each of={yearForms()}>{(f) => formRow(f())}</Each>
                  </If>
                </If>,
              )}
              <If when={(picker.yd()?.asNeeded.length || 0) > 0}>
                {section(() => "As needed", <Each of={picker.yd()?.asNeeded || []}>{(f) => asNeededRows(f())}</Each>)}
              </If>
            </If>
          </If>
          <If when={tab() === "returns"}>
            <Text class={`text-xs ${C.fg2}`}>Every saved, queued and filed return of this branch. Filed returns stay exactly as submitted.</Text>
            {section(
              () => "Returns",
              <If when={picker.returns().length > 0} else={<View class="p-3"><Text class={`text-sm ${C.fg2}`}>Saved and filed returns show here.</Text></View>}>
                <Each of={picker.returns()}>{(r) => returnRow(r())}</Each>
              </If>,
            )}
          </If>
          <If when={tab() === "library"}>
            <Text class={`text-xs ${C.fg2}`}>A blank sample of any form for this taxpayer. Save makes it your return for its period.</Text>
            <View class="flex-row flex-wrap items-center gap-2">
              <Input class={C.input} style={{ width: 260 }} placeholder="Find a form (1601C, VAT…)" value={libQ()} onChange={(e) => libQ.set(typed(e))} autoCapitalize="none" autoCorrect={false} />
              <Each of={["owned", "all"] as ("owned" | "all")[]}>
                {(sc) => (
                  <View class={`px-3 py-1 rounded-md ${libScope() === sc() ? C.accentSoft : C.inset}`} activeStyle={{ opacity: 0.7 }} onPress={() => libScope.set(sc())}>
                    <Text class={`text-sm ${libScope() === sc() ? C.accent : C.fg}`}>{sc() === "owned" ? `Filed in ${picker.year() ?? "this year"} (${picker.yd()?.forms.length || 0})` : `All forms (${picker.data()?.index.length || 0})`}</Text>
                  </View>
                )}
              </Each>
              <Each of={["All", "Monthly", "Quarterly", "Annual", "As needed"]}>
                {(fq) => (
                  <View class={`px-3 py-1 rounded-md ${libFreq() === fq() ? C.accentSoft : C.inset}`} activeStyle={{ opacity: 0.7 }} onPress={() => libFreq.set(fq())}>
                    <Text class={`text-sm ${libFreq() === fq() ? C.accent : C.fg}`}>{fq()}</Text>
                  </View>
                )}
              </Each>
            </View>
            <View class="flex-row flex-wrap gap-3">
              <Each of={library()}>
                {(f) => (
                  <View
                    class={`p-4 gap-2 rounded-2xl border ${C.line} ${C.pane}`}
                    style={{ width: 250 }}
                    activeStyle={{ opacity: 0.7 }}
                    onPress={() => openForm({ tin, code: f().c, year: picker.year() || new Date().getFullYear(), period: 0, sample: true })}
                  >
                    <View class="flex-row items-center gap-2">
                      <Text class={`text-base font-semibold flex-1 ${C.fg}`}>{f().c}</Text>
                      <If when={f().q < 0.8}><Chip text="Draft layout" tone="warning" /></If>
                    </View>
                    <Text class={`text-sm ${C.fg}`} numberOfLines={2}>{f().t}</Text>
                    <View class="flex-row flex-wrap gap-1.5">
                      <Chip text={f().freq} />
                      <Chip text={`${f().pages}p`} />
                      <Chip text={`${f().n} fields`} />
                    </View>
                  </View>
                )}
              </Each>
            </View>
            <If when={library().length === 0}>
              <Text class={`text-sm ${C.fg2}`}>{libScope() === "owned" && !(picker.yd()?.forms.length) ? "No forms for this year yet. Choose All forms." : "No form matches. Try a code like 1601C or a word like VAT."}</Text>
            </If>
          </If>
        </If>
      </View></Scroll>
    </View>
  );
}

// =====================================================================
// Root
// =====================================================================

/** `2026-Q3`, `2026-01`, `2025`, `2026-10-26`: the year, the page's period and, for a day, that it is one. */
function readTokenPeriod(text: string): { year: number; period: number; day: boolean } | null {
  const m = /^(\d{4})(?:-(?:Q([1-4])|(\d{2}))(?:-(\d{2}))?)?$/i.exec(String(text).trim());
  if (!m) return null;
  const year = Number(m[1]);
  if (m[2]) return { year, period: Number(m[2]), day: false };
  if (m[3]) return { year, period: Number(m[3]), day: Boolean(m[4]) };
  return { year, period: 0, day: false };
}

/** Rox's `/taxform` props: `form` + `period` + `action` (+ `draft_id`) open that return. */
async function followReturn(p: RoxProps) {
  const code = normCode(p.form);
  const action = p.action || "file";
  const flags = { viewOnly: action === "view", submitAsk: action === "submit" };
  const at = readTokenPeriod(String(p.period ?? ""));
  picker.focus(code, at?.year ?? null);
  // A stored return by its id: the very one, keyed as the forms list keys it.
  const id = p.draft_id == null || p.draft_id === "" ? null : Number(p.draft_id);
  if (id != null && !Number.isNaN(id)) {
    try {
      const row = (await H.returns("all")).find((r) => Number(r.id) === id && normCode(r.form_code) === code);
      if (row) return openForm({ tin: TIN(), code, year: row.taxable_year, period: row.period, ...(row.return_key != null ? { returnKey: row.return_key } : {}), ...flags });
    } catch (e) {
      toast(errText(e), "danger");
    }
  }
  if (!at) return;
  // A day names an as-needed transaction: a form keyed by its month opens on
  // that month; one keyed by the day starts as a sample (its date asked on save).
  if (at.day) {
    const freq = await H.schema(code).then((schema) => schema.freq).catch(() => "");
    if (freq !== "Monthly") return openForm({ tin: TIN(), code, year: at.year, period: 0, sample: true, ...flags });
  }
  openForm({ tin: TIN(), code, year: at.year, period: at.period, ...flags });
}

/** Deep link: `form_code` + `year` + `period` (+ `key`) open that return; `form_code` alone opens the picker on it. */
function followDeepLink() {
  const p = { ...roxProps() };
  // A return's ticket names its form and period.
  const subj = subject();
  if (!p.form && !p.form_code && subj?.kind === "return" && subj.form) {
    p.form = subj.form;
    p.period = subj.period;
  }
  if (p.form) {
    followReturn(p).catch((e) => toast(errText(e), "danger"));
    return;
  }
  if (!p.form_code) return;
  const code = normCode(p.form_code);
  const num = (v: unknown) => (v === undefined || v === null || v === "" || Number.isNaN(Number(v)) ? null : Number(v));
  const year = num(p.year);
  const period = num(p.period);
  const key = num(p.key);
  picker.focus(code, year);
  if (year != null && period != null) openForm({ tin: TIN(), code, year, period, ...(key != null ? { returnKey: key } : {}) });
}

// Rox's nsh ignores contentContainerClass: each Scroll's padding lives on a View inside it.
/** Why the ticket did not open, if it did not. */
const opening = signal("");

/** What the props' ticket stands for, from BIR; a return's ticket also gets its branch's own ticket. */
async function openTicket(ticket: string) {
  try {
    const r: any = await rox.call("bir_ticket_subject", { ticket });
    const s = r?.result as Subject;
    if (s.kind === "return") {
      const issued: any = await rox.call("bir_ticket_issue", { ticket, tin: s.tin });
      BRANCH = String(issued?.result?.ticket || "");
    } else BRANCH = ticket;
    subject.set(s);
    followDeepLink();
  } catch (e) {
    opening.set(errText(e));
  }
}

export default function R() {
  const ticket = String(roxProps().ticket || "");
  if (ticket) openTicket(ticket);
  return (
    <View style={{ flex: 1 }} class={C.pane}>
      <If
        when={TIN().length >= 9}
        else={
          <View style={{ flex: 1 }} class="items-center justify-center p-8 gap-2">
            <If
              when={!ticket}
              else={<Text class={`text-sm ${opening() ? C.danger : C.fg2}`}>{opening() || "Opening the taxpayer's forms…"}</Text>}
            >
              <Text class={`text-lg font-semibold ${C.fg}`}>Open this from a taxpayer</Text>
              <Text class={`text-sm ${C.fg2}`}>BIR Forms works on one taxpayer at a time. Ask for a taxpayer's forms in the chat to open it for them.</Text>
            </If>
          </View>
        }
      >
        <View style={{ flex: 1, display: view() === "picker" ? "flex" : "none" }}>
          <FormsPicker />
        </View>
        {() => {
          const s = view() === "form" ? cur() : null;
          return untracked(() => (s ? <FormPage s={s} /> : null));
        }}
        <Overlays />
        <ToastView />
      </If>
    </View>
  );
}
