// BIR answer cards about deadlines (spec 9.8): what's due next (dues), the
// next seven days for every taxpayer (week), one month (month), returns already late (overdue) and what filing late costs
// (penalty, recomputed as the user types, from BIR's own penalty engine).
// Buttons open the return in BIR Forms or the full Calendar; nothing here
// changes BIR's data.

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
  }
}
if (!P.snapshot && P.tool) refresh();

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

/** The card's frame: title and subtitle, the body, and its notes. The
 *  body takes the height it needs (it scrolls only past the card's), so
 *  the buttons sit right under it and a short card leaves no gap above
 *  them; each card app declares a height that fits its cards. */
function Card(p: { title: string; sub?: string; children?: any; foot?: any }) {
  return (
    <View style={{ flex: 1 }} class={`${K.pane} p-3 gap-2`}>
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
      <Scroll style={{ flexGrow: 0, flexShrink: 1 }}>
        <View class="gap-1.5">{p.children}</View>
      </Scroll>
      <If when={Boolean(p.foot)}>
        <View class="flex-row flex-wrap gap-2">{p.foot}</View>
      </If>
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

const DONE = ["filed", "confirmed", "paid"];
const events = () => ((snap()?.events || []) as any[]).filter((e) => e.enabled !== false);

// ------------------------------------------------------------ dues

function Dues() {
  const open = () =>
    events()
      .filter((e) => !DONE.includes(e.status))
      .sort((a, b) => Number(b.overdue) - Number(a.overdue) || String(a.due).localeCompare(String(b.due)));
  const when = (e: any) => {
    const n = daysTo(e.due);
    if (e.overdue) return `late since ${shortDate(e.due)}`;
    if (n === 0) return "due today";
    if (n === 1) return "due tomorrow";
    return `due ${shortDate(e.due)} · in ${n} days`;
  };
  return (
    <Card title="What's due" sub={P.ticket ? open()[0]?.taxpayer || "" : "All taxpayers"}
      foot={<Btn label="Open the Calendar" onPress={() => openApp("calendar", P.ticket)} />}>
      <If when={open().length === 0}><Empty text="Nothing due: every return on the calendar is filed." /></If>
      <Each of={open().slice(0, 12)} key={(e) => e.key}>
        {(e) => (
          <Row
            title={P.ticket ? e().label : `${e().label} · ${e().taxpayer}`}
            sub={`${when(e())} · ${e().status_detail || statusChip(e().status)[0]}`}
            right={
              <View class="flex-row gap-2 items-center">
                <Chip text={e().overdue ? "Late" : daysTo(e().due) <= 7 ? "Soon" : statusChip(e().status)[0]} tone={e().overdue ? "danger" : daysTo(e().due) <= 7 ? "warn" : "plain"} />
                <Btn label="Open" onPress={() => openApp("forms", e().key, { form: e().form, period: e().period, action: "file" })} />
              </View>
            }
          />
        )}
      </Each>
    </Card>
  );
}

// ------------------------------------------------------------ month

function Month() {
  const from = () => String(snap()?.from || "");
  const [y, m] = [Number(from().slice(0, 4)) || new Date().getFullYear(), Number(from().slice(5, 7)) || new Date().getMonth() + 1];
  const first = new Date(y, m - 1, 1).getDay();
  const days = new Date(y, m, 0).getDate();
  const picked = signal<number | null>(null);
  const on = (d: number) => events().filter((e) => Number(String(e.due).slice(8, 10)) === d && Number(String(e.due).slice(5, 7)) === m);
  const cells = () => [...Array(first).fill(0), ...Array.from({ length: days }, (_, i) => i + 1)];
  return (
    <Card title={`${MONTHS[m - 1]} ${y}`} sub="Tap a day to see what's due." foot={<Btn label="Open the full Calendar" onPress={() => openApp("calendar", P.ticket)} />}>
      <View class="flex-row flex-wrap" style={{ width: 7 * 36 }}>
        <Each of={["S", "M", "T", "W", "T", "F", "S"]}>{(d) => <Text class={`text-xs text-center ${K.fg3}`} style={{ width: 36 }}>{d()}</Text>}</Each>
        <Each of={cells()}>
          {(d) => {
            const due = d() ? on(d()) : [];
            const late = due.some((e) => e.overdue && !DONE.includes(e.status));
            return (
              <View
                style={{ width: 36, height: 30 }}
                class={`items-center justify-center rounded-md ${picked() === d() && d() ? K.infoBg : ""}`}
                onPress={d() ? () => picked.set(d()) : undefined}
                focusable={Boolean(d())}
              >
                <Text class={`text-sm ${due.length ? `font-semibold ${late ? K.danger : K.accent}` : K.fg}`}>{d() ? String(d()) : ""}</Text>
                <If when={due.length > 0}><View class={`rounded-full ${late ? "bg-[#c62828] dark:bg-[#ff6b6b]" : K.accentBg}`} style={{ width: 4, height: 4 }} /></If>
              </View>
            );
          }}
        </Each>
      </View>
      <If when={picked() !== null}>
        <Each of={on(picked() || 0)} key={(e) => e.key}>
          {(e) => <Row title={`${e().label} · ${e().taxpayer}`} sub={statusChip(e().status)[0]} right={<Btn label="Open" onPress={() => openApp("forms", e().key, { form: e().form, period: e().period, action: "file" })} />} />}
        </Each>
        <If when={on(picked() || 0).length === 0}><Empty text="Nothing due that day." /></If>
      </If>
    </Card>
  );
}

// ------------------------------------------------------------ overdue

function Overdue() {
  const items = () => (snap()?.items || []) as any[];
  return (
    <Card title="Late returns" sub={items().length ? `${items().length} past their due date` : "Nothing is late"}>
      <If when={items().length === 0}><Empty text="Every return is filed on time." /></If>
      <Each of={items()} key={(e) => e.key}>
        {(e) => (
          <Row
            title={`${e().label} · ${e().taxpayer}`}
            sub={`${e().days_late} days late · due ${shortDate(e().due)} · filing today adds surcharge, interest and compromise`}
            right={<Btn label="Open and file now" primary onPress={() => openApp("forms", e().key, { form: e().form, period: e().period, action: "file" })} />}
          />
        )}
      </Each>
    </Card>
  );
}

// ------------------------------------------------------------ penalty

function Penalty() {
  const s = () => snap() || {};
  const tax = signal(String(s().tax_due ?? ""));
  const due = signal(String(s().due || ""));
  const filed = signal(String(s().filed_on || ""));
  let timer: any;
  const recompute = () => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      const args: Record<string, unknown> = { form: s().form, due: due(), filed_on: filed(), tax_due: tax() || "0" };
      act(async () => snap.set(answerOf(await rox.call("bir_penalty", args as any))));
    }, 350);
  };
  const line = (label: string, value: unknown, strong?: boolean) => (
    <View class="flex-row justify-between px-2 py-1">
      <Text class={`text-sm ${strong ? `font-semibold ${K.fg}` : K.fg2}`}>{label}</Text>
      <Text class={`text-sm ${strong ? `font-semibold ${K.fg}` : K.fg}`}>{peso(value)}</Text>
    </View>
  );
  return (
    <Card title={`${s().form || ""} filed ${s().days_late || 0} days late`} sub="An estimate from BIR's penalty rules; BIR assesses the final amount."
      foot={P.ticket ? <Btn label="Use for a real return" onPress={() => openApp("forms", P.ticket, s().period ? { form: s().form, period: s().period, action: "file" } : { form_code: s().form })} /> : undefined}>
      <View class="flex-row flex-wrap gap-2">
        <Field label="Tax due (₱)" value={tax()} onChange={(v) => (tax.set(v), recompute())} placeholder="0.00" />
        <Field label="Due date" value={due()} onChange={(v) => (due.set(v), recompute())} placeholder="2026-10-10" width={120} />
        <Field label="Filed and paid on" value={filed()} onChange={(v) => (filed.set(v), recompute())} placeholder="2026-12-10" width={120} />
      </View>
      <View class={`rounded-lg ${K.soft}`}>
        {line("Surcharge", s().surcharge)}
        {line("Interest", s().interest)}
        {line("Compromise", s().compromise)}
        {line("Penalties", s().total_penalties, true)}
        {line("Tax and penalties", s().total_to_pay, true)}
      </View>
      <Text class={`text-xs ${K.fg3}`}>{s().basis || ""}</Text>
    </Card>
  );
}

// ------------------------------------------------------------ week

function Week() {
  const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const open = () => events().sort((a, b) => String(a.due).localeCompare(String(b.due)) || String(a.taxpayer).localeCompare(String(b.taxpayer)));
  const days = () => [...new Set(open().map((e) => String(e.due)))];
  const left = () => open().filter((e) => !DONE.includes(e.status));
  const who = () => new Set(left().map((e) => e.tin)).size;
  const label = (day: string) => {
    const [y, m, d] = day.split("-").map(Number);
    const n = daysTo(day);
    return `${DAYS[new Date(y, m - 1, d).getDay()]}, ${shortDate(day)}${n === 0 ? " · today" : n === 1 ? " · tomorrow" : ""}`;
  };
  return (
    <Card title="This week" sub={`${left().length} to file · ${who()} taxpayer${who() === 1 ? "" : "s"}`}
      foot={<Btn label="Open the Calendar" onPress={() => openApp("calendar", P.ticket)} />}>
      <If when={open().length === 0}><Empty text="Nothing is due in the next seven days." /></If>
      <Each of={days()} key={(d) => d}>
        {(d) => (
          <View class="gap-1">
            <Text class={`text-xs font-semibold pt-1 ${K.fg2}`}>{label(d())}</Text>
            <Each of={open().filter((e) => String(e.due) === d())} key={(e) => e.key}>
              {(e) => (
                <Row title={`${e().form} · ${e().taxpayer}`} sub={String(e().label || "").replace(`${e().form} · `, "")}
                  right={
                    <View class="flex-row gap-2 items-center">
                      <Chip text={statusChip(e().status)[0]} tone={statusChip(e().status)[1]} />
                      <If when={!DONE.includes(e().status)}><Btn label="Open" onPress={() => openApp("forms", e().key, { form: e().form, period: e().period, action: "file" })} /></If>
                    </View>
                  } />
              )}
            </Each>
          </View>
        )}
      </Each>
    </Card>
  );
}

export default function DatesCard() {
  if (P.card === "week") return <Week />;
  if (P.card === "month") return <Month />;
  if (P.card === "overdue") return <Overdue />;
  if (P.card === "penalty") return <Penalty />;
  return <Dues />;
}
