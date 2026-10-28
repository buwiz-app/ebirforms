// BIR answer cards about returns (spec 9.8): a taxpayer's forms (forms),
// the year at a glance (matrix), one return's path (status), what blocks a
// return (needs) and the approval before the assistant sends (approve).
// Buttons open BIR Forms on the return's ticket; "Allow this once" and
// "Mark paid" run only when the user presses them (origin app).

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

/** The card's frame: title and subtitle, the body, and its notes. */
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
      <Scroll class="flex-1">
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
async function uploadReceipt(): Promise<{ upload: string; name: string } | null> {
  const files = await rox.pickFile({ extensions: [".pdf", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".heic"], multiple: false });
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
const FREQ_ORDER = ["monthly", "quarterly", "annual", "as needed", ""];

// ------------------------------------------------------------ forms

function Forms() {
  const s = () => snap() || {};
  const forms = () => (s().registered || []) as any[];
  /** The form's period that matters now: the first not filed, else the last. */
  const current = (f: any) => ((f.periods || []) as any[]).find((p) => !DONE.includes(p.status)) || (f.periods || []).slice(-1)[0];
  const urgent = () =>
    forms()
      .map((f) => ({ f, p: current(f) }))
      .filter((x) => x.p && !DONE.includes(x.p.status))
      .sort((a, b) => String(a.p.due).localeCompare(String(b.p.due)))[0];
  const open = (code: string, period?: string) => openApp("forms", P.ticket, period ? { form: code, period, action: "file" } : { form_code: code });
  const groups = () => FREQ_ORDER.map((freq) => ({ freq, items: forms().filter((f) => String(f.frequency || "") === freq) })).filter((g) => g.items.length);
  return (
    <Card
      title={`${s().taxpayer || "Taxpayer"} · forms`}
      sub={`${s().year || ""} · ${forms().length} registered`}
      foot={[
        <If when={Boolean(urgent())}>
          <Btn label={`File ${urgent()?.p.label || urgent()?.f.code}`} primary onPress={() => open(urgent()!.f.code, urgent()!.p.period)} />
        </If>,
        <Btn label="Schedule" onPress={() => openApp("calendar", P.ticket)} />,
        <Btn label="Add a form" onPress={() => openApp("tax-profile", P.ticket)} />,
      ]}
    >
      <If when={forms().length === 0}>
        <Empty text="No forms are chosen on this profile for the year. Add them in the Tax Profile." />
      </If>
      <Each of={groups()} key={(g) => g.freq}>
        {(g) => (
          <View class="gap-1">
            <Text class={`text-xs font-semibold pt-1 ${K.fg2}`}>{(g().freq || "other").replace(/^./, (c: string) => c.toUpperCase())}</Text>
            <Each of={g().items} key={(f) => f.code}>
              {(f) => {
                const p = current(f());
                const [word, tone] = p ? statusChip(p.status) : ["No period yet", "plain" as Tone];
                return (
                  <Row
                    title={`${f().code} · ${f().title || ""}`}
                    sub={p ? `${String(p.label).replace(`${f().code} · `, "")} · due ${shortDate(p.due)}${p.overdue && !DONE.includes(p.status) ? " · late" : ""}` : ""}
                    right={
                      <View class="flex-row gap-2 items-center">
                        <Chip text={p?.overdue && !DONE.includes(p.status) ? "Late" : word} tone={p?.overdue && !DONE.includes(p.status) ? "danger" : tone} />
                        <Btn label="Open" onPress={() => open(f().code, p?.period)} />
                      </View>
                    }
                  />
                );
              }}
            </Each>
          </View>
        )}
      </Each>
    </Card>
  );
}

// ------------------------------------------------------------ matrix

/** The month a return's period ends in (1-12), for its column. */
function monthOf(period: string): number {
  const q = /Q(\d)$/.exec(period);
  if (q) return Number(q[1]) * 3;
  const m = /^\d{4}-(\d{2})/.exec(period);
  return m ? Number(m[1]) : 12;
}

function Matrix() {
  const s = () => snap() || {};
  const returns = () => (s().returns || []) as any[];
  const forms = () => [...new Set(returns().map((r) => String(r.form)))].sort();
  const cell = (form: string, month: number) => returns().find((r) => r.form === form && monthOf(String(r.period)) === month);
  const tone = (r: any): string => {
    if (!r) return "bg-black/5 dark:bg-white/5";
    if (r.paid) return "bg-[#2e7d32] dark:bg-[#81c784]";
    if (String(r.status) === "Confirmed") return "bg-[#1680f3] dark:bg-[#1884fc]";
    if (r.filed) return "bg-[#1680f399] dark:bg-[#1884fc99]";
    return "bg-[#b26a00] dark:bg-[#ffb74d]";
  };
  return (
    <Card
      title={`${s().taxpayer || "Taxpayer"} · ${s().year || "returns"}`}
      sub="Paid · confirmed · sent · draft. Tap a square to open it."
      foot={<Btn label="Open Calendar" onPress={() => openApp("calendar", P.ticket)} />}
    >
      <If when={returns().length === 0}>
        <Empty text="No returns saved for this year yet." />
      </If>
      <If when={returns().length > 0}>
        <View class="flex-row gap-1 pl-14">
          <Each of={MONTHS}>{(m) => <Text class={`text-xs text-center ${K.fg3}`} style={{ width: 26 }}>{m().slice(0, 1)}</Text>}</Each>
        </View>
      </If>
      <Each of={forms()} key={(f) => f}>
        {(f) => (
          <View class="flex-row items-center gap-1">
            <Text class={`text-xs font-semibold ${K.fg}`} style={{ width: 52 }}>{f()}</Text>
            <Each of={MONTHS.map((_, i) => i + 1)}>
              {(m) => {
                const r = cell(f(), m());
                return (
                  <View
                    class={`rounded ${tone(r)}`}
                    style={{ width: 26, height: 20 }}
                    onPress={r ? () => openApp("forms", ticketOf(r.token)) : undefined}
                    focusable={Boolean(r)}
                    accessibilityLabel={r ? `${f()} ${r.period} ${r.status}` : undefined}
                  />
                );
              }}
            </Each>
          </View>
        )}
      </Each>
      <If when={Boolean(s().more)}><Text class={`text-xs ${K.fg3}`}>More returns than fit here: Refresh reads them all.</Text></If>
    </Card>
  );
}

// ------------------------------------------------------------ status

const PATH = ["Draft", "Queued", "Submitted", "Confirmed", "Paid"];
/** A field id in words: `item_13_election` -> "Item 13 election". */
const human = (field: unknown) => String(field || "").replace(/^frm\w+:/, "").replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());

function Status() {
  const s = () => snap() || {};
  const r = () => ((s().returns || []) as any[])[0];
  const at = () => (r() ? (r().paid ? 4 : Math.max(0, PATH.indexOf(String(r().status)))) : -1);
  const markPaid = () =>
    act(() => rox.call("bir_mark_paid", { tin: P.ticket || "", form: r().form, period: r().period }), "Marked as paid.").then(refresh);
  const next = () => {
    if (!r()) return null;
    if (at() === 0) return <Btn label="Continue" primary onPress={() => openApp("forms", ticketOf(r().token))} />;
    if (at() >= 2 && at() < 4) return <Btn label="Mark paid" primary onPress={markPaid} />;
    return null;
  };
  return (
    <Card
      title={r() ? String(r().label || `${r().form} · ${r().period}`) : "Return"}
      sub={s().taxpayer || ""}
      foot={[next(), r() ? <Btn label="Open in Forms" onPress={() => openApp("forms", ticketOf(r().token))} /> : null]}
    >
      <If when={!r()}>
        <Empty text="No return of that form and period is saved yet." />
      </If>
      <If when={Boolean(r())}>
        <View class="flex-row items-center gap-1 py-2">
          <Each of={PATH}>
            {(step, i) => (
              <View class="flex-row items-center gap-1">
                <Chip text={step() === "Submitted" ? "Sent" : step()} tone={i() < at() || (i() === at() && r()?.paid) ? "ok" : i() === at() ? "info" : "plain"} />
                <If when={i() < PATH.length - 1}><Text class={`text-xs ${K.fg3}`}>›</Text></If>
              </View>
            )}
          </Each>
        </View>
        <Row title="Last saved" sub={String(r()?.last_saved || "").slice(0, 16).replace("T", " ")} />
      </If>
    </Card>
  );
}

// ------------------------------------------------------------ needs

function Needs() {
  const s = () => snap() || {};
  const items = () => [
    ...((s().boxes || []) as any[]).map((b) => ({ title: `${b.item ? `Item ${b.item} · ` : ""}${b.label || human(b.field)}`, sub: b.error || (b.required ? "Required" : "") })),
    ...((s().other_errors || []) as any[]).map((e) => ({ title: human(e.field) || "The return", sub: e.message })),
  ];
  return (
    <Card
      title={`${s().form || "Return"} · what it needs`}
      sub={s().ok ? "Nothing blocks it." : `${items().length} to fix before it can be sent`}
      foot={[
        <Btn label="Fix in Forms" primary onPress={() => openApp("forms", s().tin, s().form ? { form_code: s().form } : {})} />,
        <Btn label="Check again" onPress={refresh} />,
      ]}
    >
      <If when={items().length === 0}><Empty text="Every box checks out." /></If>
      <Each of={items()}>{(it) => <Row title={it().title} sub={it().sub} />}</Each>
    </Card>
  );
}

// ------------------------------------------------------------ approve

function Approve() {
  const s = () => snap() || {};
  const done = signal<"" | "allowed" | "later">("");
  const allow = () =>
    act(() => rox.call("bir_card_allow_send", { tin: s().tin || "", form: s().form || "" })).then(() => {
      if (!failure()) done.set("allowed");
    });
  return (
    <Card
      title={`Send ${s().form || "this return"} to BIR?`}
      sub="The assistant can't send it without you."
      foot={
        done() === ""
          ? [
              <Btn label="Allow this once" primary onPress={allow} />,
              <Btn label="Review in Forms" onPress={() => openApp("forms", s().tin, s().form ? { form_code: s().form } : {})} />,
              <Btn label="Not now" onPress={() => done.set("later")} />,
            ]
          : undefined
      }
    >
      <Text class={`text-sm ${K.fg}`}>
        {done() === "allowed"
          ? "Allowed once. Ask the assistant to send it again; the permission ends with that send."
          : done() === "later"
            ? "Not sent. The return stays a draft."
            : "Allow this once lets the assistant queue this return for BIR on its next try. Review it in Forms first if you're not sure."}
      </Text>
    </Card>
  );
}

export default function FormsCard() {
  if (P.card === "matrix") return <Matrix />;
  if (P.card === "status") return <Status />;
  if (P.card === "needs") return <Needs />;
  if (P.card === "approve") return <Approve />;
  return <Forms />;
}
