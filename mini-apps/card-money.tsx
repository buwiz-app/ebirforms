// BIR answer cards about payments (spec 9.8): returns not paid (unpaid),
// what was paid this year (paidsum) and what the routines recorded since
// yesterday (digest). Every button that records something runs only when
// the user presses it (`rox.call`, origin app); the assistant can't press
// them. (match, markpaid and confirm moved to the short card-quick app.)

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

const today = () => {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
};

/** Mark one return paid: with details through bir_record_payment, without through bir_mark_paid. */
async function pay(ret: { tin: string; form: string; period: string }, d?: { paid_at: string; amount: string; channel: string; reference: string }) {
  if (d && d.amount.trim() && d.channel.trim())
    return rox.call("bir_record_payment", {
      tin: ret.tin, form: ret.form, period: ret.period, channel: d.channel.trim(), amount: d.amount.trim(), paid_at: d.paid_at.trim() || today(),
      ...(d.reference.trim() ? { reference: d.reference.trim() } : {}),
    });
  return rox.call("bir_mark_paid", { tin: ret.tin, form: ret.form, period: ret.period });
}
async function attachProof(ret: { tin: string; form: string; period: string }) {
  const proof = await uploadReceipt();
  if (!proof) return;
  await rox.call("bir_mark_paid", { tin: ret.tin, form: ret.form, period: ret.period, proof_upload: proof.upload, proof_name: proof.name });
}

// ------------------------------------------------------------ unpaid

function Unpaid() {
  const items = () => (snap()?.items || []) as any[];
  const paid = signal<string[]>([]);
  const key = (r: any) => `${r.tin}/${r.form}/${r.period}`;
  return (
    <Card title="Not paid yet" sub={`${items().length} sent or confirmed with no payment recorded`}>
      <If when={items().length === 0}><Empty text="Every filed return has its payment recorded." /></If>
      <Each of={items()} key={(r) => key(r)}>
        {(r) => (
          <Row
            title={`${r().form} · ${r().period} · ${r().taxpayer}`}
            sub={paid().includes(key(r())) ? "Marked paid" : statusChip(r().status)[0]}
            right={
              <If when={!paid().includes(key(r()))}>
                <View class="flex-row gap-2">
                  <Btn label="Mark as paid" primary onPress={() => act(() => pay(r()), "Marked as paid.").then(() => !failure() && paid.set([...paid(), key(r())]))} />
                  <Btn label="Attach receipt" onPress={() => act(() => attachProof(r()), "Receipt attached.").then(() => !failure() && paid.set([...paid(), key(r())]))} />
                </View>
              </If>
            }
          />
        )}
      </Each>
    </Card>
  );
}

// ------------------------------------------------------------ paidsum

function PaidSum() {
  const s = () => snap() || {};
  const t = () => s().totals || {};
  const months = () => {
    const year = String(s().year || new Date().getFullYear());
    const by = new Map(((t().by_month || []) as any[]).map((m) => [String(m.month), Number(m.amount) || 0]));
    return MONTHS.map((name, i) => ({ name, amount: by.get(`${year}-${String(i + 1).padStart(2, "0")}`) || 0 }));
  };
  const top = () => Math.max(1, ...months().map((m) => m.amount));
  return (
    <Card title={`Paid${s().year ? ` in ${s().year}` : ""}${s().taxpayer ? ` · ${s().taxpayer}` : ""}`} sub={`${t().count || 0} payment${t().count === 1 ? "" : "s"} recorded`}>
      <Text class={`text-2xl font-semibold ${K.fg}`}>{peso(t().all || 0)}</Text>
      <View class="flex-row items-end gap-1 py-1" style={{ height: 52 }}>
        <Each of={months()}>
          {(m) => (
            <View class="items-center gap-0.5">
              <View class={`rounded-sm ${m().amount ? K.accentBg : "bg-black/5 dark:bg-white/10"}`} style={{ width: 22, height: Math.max(3, Math.round((m().amount / top()) * 36)) }} />
              <Text class={`text-xs text-center ${K.fg3}`} style={{ width: 22 }}>{m().name.slice(0, 1)}</Text>
            </View>
          )}
        </Each>
      </View>
      <Each of={((t().by_form || []) as any[]).slice(0, 5)} key={(f) => f.form}>
        {(f) => <Fact label={String(f().form)} value={peso(f().amount)} />}
      </Each>
    </Card>
  );
}

// ------------------------------------------------------------ digest

function Digest() {
  const s = () => snap() || {};
  const items = () => (s().items || []) as any[];
  const done = signal<string[]>([]);
  const chip: Record<string, [string, Tone]> = { confirmed: ["Confirmed", "ok"], payment: ["Paid", "info"], waiting: ["Look", "warn"] };
  const markPaid = (r: any) =>
    act(() => rox.call("bir_mark_paid", { tin: r.tin, form: r.form, period: r.period }), `${r.label} marked paid.`).then(() => !failure() && done.set([...done(), r.key]));
  return (
    <Card title="Since yesterday" sub={`${items().length} thing${items().length === 1 ? "" : "s"} since ${shortDate(s().since)}`}>
      <If when={items().length === 0}><Empty text="Nothing new: no confirmations or payments were recorded." /></If>
      <Each of={items()} key={(r) => `${r.kind}/${r.key}`}>
        {(r) => (
          <Row title={`${r().label} · ${r().taxpayer}`} sub={done().includes(r().key) ? "Marked paid" : String(r().detail || "")}
            right={
              <View class="flex-row gap-2 items-center">
                <Chip text={(chip[r().kind] || ["", "plain"])[0]} tone={(chip[r().kind] || ["", "plain"])[1]} />
                {r().kind === "confirmed" && !r().paid && !done().includes(r().key)
                  ? <Btn label="Mark paid" primary onPress={() => markPaid(r())} />
                  : <Btn label="Open" onPress={() => openApp("forms", r().key)} />}
              </View>
            } />
        )}
      </Each>
    </Card>
  );
}

export default function MoneyCard() {
  if (P.card === "paidsum") return <PaidSum />;
  if (P.card === "digest") return <Digest />;
  return <Unpaid />;
}
