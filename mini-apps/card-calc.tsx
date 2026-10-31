// BIR answer cards for quick lookups (spec 9.8): an ATC (atc), a revenue
// district office (rdo) and the tax on an amount (rate, recomputed here as
// the user changes the amount). They read BIR's own tables through
// bir_lookup and change nothing.

import { signal, computed, Text, View, Scroll, Input } from "nsh";
import { rox } from "rox";

// card-kit:begin
// BIR's answer-card kit (spec 9.8), the same in every card app: an app may
// import only nsh, nsh/base and rox, so each card app carries this block
// verbatim (v1-tests/apps.test.mjs checks it against card-kit.tsx.in).
// The card's props: which card, the answer's snapshot and when it was
// taken, and the tool and arguments Refresh re-reads it with.

type CardProps = { card?: string; ticket?: string; snapshot?: any; as_of?: number; tool?: string; args?: Record<string, unknown> };
const P = (rox.props || {}) as CardProps;

const K = {
  pane: "bg-white dark:bg-[#161616]",
  soft: "bg-[#f5f5f5] dark:bg-[#1f1f1f]",
  line: "border-black/10 dark:border-white/10",
  fg: "text-[#1c1c1c] dark:text-[#f5f5f5]",
  fg2: "text-[#1c1c1c99] dark:text-[#f5f5f599]",
  fg3: "text-[#1c1c1c66] dark:text-[#f5f5f566]",
  accent: "text-[#1680f3] dark:text-[#1884fc]",
  accentBg: "bg-[#1680f3] dark:bg-[#1884fc]",
  danger: "text-[#c62828] dark:text-[#ff6b6b]",
  dangerBg: "bg-[#c628281a] dark:bg-[#ff6b6b26]",
  warn: "text-[#b26a00] dark:text-[#ffb74d]",
  warnBg: "bg-[#b26a001a] dark:bg-[#ffb74d26]",
  ok: "text-[#2e7d32] dark:text-[#81c784]",
  okBg: "bg-[#2e7d321a] dark:bg-[#81c78426]",
  infoBg: "bg-[#1680f31a] dark:bg-[#1884fc26]",
};
type Tone = "ok" | "warn" | "danger" | "info" | "plain";
const toneOf: Record<Tone, [string, string]> = {
  ok: [K.okBg, K.ok],
  warn: [K.warnBg, K.warn],
  danger: [K.dangerBg, K.danger],
  info: [K.infoBg, K.accent],
  plain: ["bg-black/5 dark:bg-white/10", K.fg2],
};

/** A tool's answer as data: its structured result, or its text as JSON. */
function answerOf(r: any): any {
  if (r && typeof r === "object" && "result" in r) return r.result;
  if (typeof r === "string") {
    try {
      return JSON.parse(r);
    } catch {
      return r;
    }
  }
  return r;
}
const errText = (e: unknown) => String((e as any)?.message || e || "Something went wrong").replace(/^Error:\s*/, "");

/** The snapshot, re-read live when the card mounts without one. */
const snap = signal<any>(P.snapshot ?? null);
const busy = signal(false);
const failure = signal("");
const note = signal("");

/** Tells Rox the card's data is in (`rox.ready`): the manifest's
 *  `inline.wait_ready` holds the card's placeholder until then. A Rox
 *  without it has no `rox.ready`. */
const ready = () => (rox as any).ready?.();

async function refresh() {
  if (!P.tool || busy()) return;
  busy.set(true);
  failure.set("");
  try {
    snap.set(answerOf(await rox.call(P.tool, (P.args || {}) as any)));
  } catch (e) {
    failure.set(errText(e));
  } finally {
    busy.set(false);
    ready();
  }
}
if (!P.snapshot && P.tool) refresh();
else ready();

/** Run a button's call; its error shows on the card. */
async function act(run: () => Promise<unknown>, done?: string) {
  if (busy()) return;
  busy.set(true);
  failure.set("");
  try {
    await run();
    if (done) note.set(done);
  } catch (e) {
    failure.set(errText(e));
  } finally {
    busy.set(false);
  }
}
const openApp = (app: string, ticket?: string, props: Record<string, unknown> = {}) =>
  act(() => rox.open(ticket ? { app, ticket, props: { ticket, ...props } } : { app, props }));
/** The ticket in a canonical token (`@bir:taxform:tkt_…`). */
const ticketOf = (token: unknown) => String(token || "").split(":").pop() || "";

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
function shortDate(day: unknown) {
  const [y, m, d] = String(day || "").slice(0, 10).split("-").map(Number);
  return y && m && d ? `${MONTHS[m - 1]} ${d}` : String(day || "");
}
function daysTo(day: unknown) {
  const [y, m, d] = String(day || "").slice(0, 10).split("-").map(Number);
  const now = new Date();
  return Math.round((new Date(y, m - 1, d).getTime() - new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime()) / 86_400_000);
}
const peso = (n: unknown) => (n == null || n === "" ? "—" : `₱${Number(n).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`);
/** A return's status as words and a tone. */
function statusChip(status: unknown, paid?: boolean): [string, Tone] {
  const s = String(status || "").toLowerCase();
  if (paid || s === "paid") return ["Paid", "ok"];
  if (s === "confirmed") return ["Confirmed", "ok"];
  if (s === "filed" || s === "submitted") return ["Sent", "info"];
  if (s === "queued") return ["Queued", "info"];
  if (s === "draft") return ["Draft", "warn"];
  return ["Not started", "plain"];
}

function Chip(p: { text: string; tone?: Tone }) {
  const [bg, fg] = toneOf[p.tone || "plain"];
  return (
    <View class={`rounded-full px-2 py-0.5 ${bg}`}>
      <Text class={`text-xs font-semibold ${fg}`}>{p.text}</Text>
    </View>
  );
}

function Btn(p: { label: string; onPress: () => void; primary?: boolean; disabled?: boolean }) {
  return (
    <View
      onPress={() => !p.disabled && p.onPress()}
      focusable={!p.disabled}
      accessibilityRole="button"
      accessibilityLabel={p.label}
      activeStyle={{ opacity: 0.7 }}
      class={`rounded-md px-3 py-1 items-center justify-center border-2 border-transparent focus:border-[#1680f3] dark:focus:border-[#1884fc] ${p.primary ? K.accentBg : "bg-black/5 dark:bg-white/10"} ${p.disabled ? "opacity-40" : ""}`}
    >
      <Text class={`text-sm font-semibold ${p.primary ? "text-white dark:text-white" : K.fg}`}>{p.label}</Text>
    </View>
  );
}

/** The card's height as its content needs it, for `rox.fit`: the
 *  column's, with the body's full height in place of the part the scroll
 *  shows. The same whether or not the frame cuts the body short, so the
 *  card settles at once. Measured after a layout pass (its parts report
 *  one by one). */
const fitParts = { column: 0, shown: 0, body: 0 };
let fitSent = 0;
let fitTimer: any = null;
function measured(part: keyof typeof fitParts, e: any) {
  fitParts[part] = (e.bounds ?? e).size.height;
  if (fitTimer !== null) return;
  fitTimer = setTimeout(() => {
    fitTimer = null;
    const { column, shown, body } = fitParts;
    const h = Math.ceil(column - shown + body);
    // a Rox without rox.fit keeps the declared height
    if (!column || Math.abs(h - fitSent) < 1 || typeof rox.fit !== "function") return;
    fitSent = h;
    rox.fit(h);
  }, 0);
}

/** The card's frame: title and subtitle, the body, and its notes. Rox
 *  sizes the card to all of it (`rox.fit`, within the manifest's
 *  min_height..max_height), so a status line, an error, a note or a
 *  button that appears grows the card rather than scrolling it. Only a
 *  body past max_height (a long list) scrolls, under the title and above
 *  the buttons. */
function Card(p: { title: string; sub?: string; children?: any; foot?: any }) {
  return (
    <View style={{ flex: 1 }} class={K.pane}>
      <View style={{ flexShrink: 1, minHeight: 0 }} class="p-3 gap-2" onLayout={(e: any) => measured("column", e)}>
        <View class="flex-row items-start gap-2">
          <View class="flex-1">
            <Text class={`text-base font-semibold ${K.fg}`} numberOfLines={1}>{p.title}</Text>
            <If when={Boolean(p.sub)}>
              <Text class={`text-xs ${K.fg2}`} numberOfLines={1}>{p.sub}</Text>
            </If>
          </View>
          {/* Rox draws "as of" and Refresh above the card (spec 9.8); Refresh
              remounts it without a snapshot and the card reads live. */}
          <If when={busy()}>
            <Text class={`text-xs ${K.fg3}`}>Working…</Text>
          </If>
        </View>
        <If when={Boolean(failure())}>
          <Text class={`text-xs ${K.danger}`}>{failure()}</Text>
        </If>
        <If when={Boolean(note())}>
          <Text class={`text-xs ${K.ok}`}>{note()}</Text>
        </If>
        <Scroll style={{ flexGrow: 0, flexShrink: 1, minHeight: 0 }} onLayout={(e: any) => measured("shown", e)}>
          <View class="gap-1.5" onLayout={(e: any) => measured("body", e)}>{p.children}</View>
        </Scroll>
        <If when={Boolean(p.foot)}>
          <View class="flex-row flex-wrap gap-2">{p.foot}</View>
        </If>
      </View>
    </View>
  );
}

/** One row: a title, a line under it, and its chip and buttons on the right. */
function Row(p: { title: string; sub?: string; right?: any; onPress?: () => void }) {
  return (
    <View
      class={`flex-row items-center gap-2 px-2 py-1.5 rounded-lg ${K.soft}`}
      onPress={p.onPress}
      activeStyle={p.onPress ? { opacity: 0.7 } : undefined}
      focusable={Boolean(p.onPress)}
    >
      <View class="flex-1">
        <Text class={`text-sm font-semibold ${K.fg}`} numberOfLines={1}>{p.title}</Text>
        <If when={Boolean(p.sub)}>
          <Text class={`text-xs ${K.fg2}`} numberOfLines={2}>{p.sub}</Text>
        </If>
      </View>
      {p.right}
    </View>
  );
}

/** A label and its value on one line (a fact sheet). */
function Fact(p: { label: string; value: any; strong?: boolean }) {
  return (
    <View class="flex-row items-start gap-3 py-0.5">
      <Text class={`text-xs ${K.fg2}`} style={{ width: 110 }}>{p.label}</Text>
      <View class="flex-1">
        {typeof p.value === "string" || typeof p.value === "number" ? <Text class={`text-sm ${p.strong ? "font-semibold" : ""} ${K.fg}`}>{String(p.value)}</Text> : p.value}
      </View>
    </View>
  );
}

function Empty(p: { text: string }) {
  return <Text class={`text-sm py-3 ${K.fg2}`}>{p.text}</Text>;
}

/** A labelled input. */
function Field(p: { label: string; value: string; onChange: (v: string) => void; placeholder?: string; width?: number }) {
  return (
    <View class="gap-0.5">
      <Text class={`text-xs ${K.fg2}`}>{p.label}</Text>
      <Input
        value={p.value}
        placeholder={p.placeholder}
        onChange={(e: any) => p.onChange(typeof e === "string" ? e : e.value)}
        style={{ width: p.width || 140 }}
        class={`placeholder:text-[#8e8e93] h-8 px-2 rounded-md border ${K.line} ${K.pane} text-sm ${K.fg}`}
      />
    </View>
  );
}

/** A picked receipt sent in chunks for bir_mark_paid's `proof_upload`. */
const uploadReceipt = () => uploadFile([".pdf", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".heic"]);

/** A picked file sent in chunks (bir_app_upload), for a tool's `upload`. */
async function uploadFile(extensions: string[]): Promise<{ upload: string; name: string } | null> {
  const files = await rox.pickFile({ extensions, multiple: false });
  const file = files[0];
  if (!file) return null;
  const bytes = await file.read();
  const A = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const b64 = (b: Uint8Array) => {
    let out = "";
    for (let i = 0; i < b.length; i += 3) {
      const n = (b[i] << 16) | ((b[i + 1] ?? 0) << 8) | (b[i + 2] ?? 0);
      out += A[(n >> 18) & 63] + A[(n >> 12) & 63] + (i + 1 < b.length ? A[(n >> 6) & 63] : "=") + (i + 2 < b.length ? A[n & 63] : "=");
    }
    return out;
  };
  let upload = "";
  let at = 0;
  do {
    const r: any = await rox.call("bir_app_upload", { ...(upload ? { upload } : {}), data: b64(bytes.subarray(at, at + 180 * 1024)) });
    upload = String(answerOf(r)?.upload || "");
    at += 180 * 1024;
  } while (at < bytes.length);
  return { upload, name: file.name };
}
// card-kit:end

// ------------------------------------------------------------ atc

function Atc() {
  const s = () => snap() || {};
  const forms = () => (s().forms || []) as string[];
  return (
    <Card title={String(s().code || "ATC")} sub="Alphanumeric tax code">
      <Fact label="Covers" value={String(s().covers || "")} />
      <Fact label="Rate" value={s().rate == null ? "Set by the return" : `${s().rate}%`} strong />
      <Fact label="Used on" value={forms().length ? <View class="flex-row flex-wrap gap-1.5"><Each of={forms()}>{(f) => <Chip text={f()} tone="info" />}</Each></View> : "No form BIR prepares"} />
    </Card>
  );
}

// ------------------------------------------------------------ rdo

function Rdo() {
  const s = () => snap() || {};
  const matches = () => (s().matches || []) as any[];
  return (
    <Card title={matches().length === 1 ? `RDO ${matches()[0].code}` : "Revenue district offices"} sub={matches().length === 1 ? "Revenue district office" : `${matches().length} match`}>
      <Each of={matches()} key={(m) => m.code}>
        {(m) => (
          <Row title={`${m().code} · ${m().office}`} sub={m().taxpayers ? `${m().taxpayers} of your taxpayers are registered here` : "None of your taxpayers is registered here"} />
        )}
      </Each>
    </Card>
  );
}

// ------------------------------------------------------------ rate

function Rate() {
  const s = () => snap() || {};
  const base = signal(String(snap()?.amount ?? ""));
  const value = () => Number(base().replace(/[₱,\s]/g, ""));
  const tax = () => (Number.isFinite(value()) ? Math.round(value() * Number(s().rate || 0)) / 100 : NaN);
  return (
    <Card title={s().atc ? `${s().atc} · ${s().rate}%` : `${s().rate}% of an amount`} sub={String(s().rule || "")}>
      <View class="flex-row items-end gap-3">
        <Field label="Amount (₱)" value={base()} onChange={(v) => base.set(v)} width={160} />
        <Text class={`text-sm pb-1.5 ${K.fg2}`}>{`× ${s().rate}% =`}</Text>
        <Text class={`text-xl font-semibold pb-0.5 ${K.fg}`}>{Number.isNaN(tax()) ? "—" : peso(tax())}</Text>
      </View>
    </Card>
  );
}

export default function CalcCard() {
  if (P.card === "atc") return <Atc />;
  if (P.card === "rdo") return <Rdo />;
  return <Rate />;
}
