// BIR answer cards about taxpayers (spec 9.8): which taxpayer (pick), one
// taxpayer at a glance (taxpayer), their branches (branches), a new
// taxpayer to add (quickadd), every taxpayer's standing (board) and who
// files on time (streak).
// The assistant's tool picks the card and hands it the answer as
// `props.snapshot`; Refresh re-reads it through the same tool. Buttons
// open BIR's full apps on the taxpayer's ticket or attach the taxpayer to
// the chat (Rox asks first). The only change, quickadd's Save, runs when
// the user presses it.

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

// ------------------------------------------------------------ pick

function Pick() {
  const s = () => snap() || {};
  const matches = () => (s().matches || []) as any[];
  return (
    <Card title="Which taxpayer?" sub={`Closest to "${s().query || ""}"`}
      foot={<Btn label="Add a new taxpayer" onPress={() => openApp("register")} />}>
      <If when={matches().length === 0}>
        <Empty text="No saved taxpayer has a name like that. Check the spelling, or add them." />
      </If>
      <Each of={matches()} key={(m) => m.tin}>
        {(m) => (
          <Row
            title={m().name}
            sub={[m().exact ? "Exact match" : "Close match", m().detail].filter(Boolean).join(" · ")}
            right={
              <View class="flex-row gap-2">
                <Btn label="Use" primary onPress={() => act(() => rox.attach("taxprofile", m().tin), `${m().name} is attached to this chat.`)} />
                <Btn label="Profile" onPress={() => openApp("tax-profile", m().tin)} />
              </View>
            }
          />
        )}
      </Each>
    </Card>
  );
}

// ------------------------------------------------------------ taxpayer

function Taxpayer() {
  const s = () => snap() || {};
  const ticket = () => P.ticket || "";
  const next = () => s().next_due;
  return (
    <Card
      title={s().name || "Taxpayer"}
      sub={[s().type, s().rdo ? `RDO ${s().rdo}` : ""].filter(Boolean).join(" · ")}
      foot={[
        <Btn label="Open profile" primary onPress={() => openApp("tax-profile", ticket())} />,
        <Btn label="New return" onPress={() => openApp("forms", ticket())} />,
        <Btn label="Attach to this chat" onPress={() => act(() => rox.attach("taxprofile", ticket()), "Attached to this chat.")} />,
      ]}
    >
      <Row
        title={next() ? `Next due: ${next().label}` : "Nothing due soon"}
        sub={next() ? `${shortDate(next().due)} · in ${daysTo(next().due)} days` : "Every return on the calendar is filed."}
        right={next() ? <Btn label="Open" onPress={() => openApp("forms", ticketOf(next().key))} /> : undefined}
      />
      <Row title={`Forms in ${s().year || ""}`} sub={((s().forms || []) as string[]).join(", ") || "None chosen yet"} />
      <Text class={`text-xs font-semibold pt-1 ${K.fg2}`}>{`Branches · ${(s().branches || []).length}`}</Text>
      <Each of={(s().branches || []) as any[]} key={(b) => b.code}>
        {(b) => <Row title={`${b().head_office ? "Head office" : b().name || "Branch"} · ${b().code}`} sub={[b().rdo ? `RDO ${b().rdo}` : "", b().address].filter(Boolean).join(" · ")} />}
      </Each>
    </Card>
  );
}

// ------------------------------------------------------------ board

function Board() {
  const s = () => snap() || {};
  const lateOnly = signal(false);
  const rows = () => ((s().taxpayers || []) as any[]).filter((t) => !lateOnly() || t.late > 0);
  return (
    <Card
      title="My taxpayers"
      sub={`This year · done, due within two weeks, late`}
      foot={<Btn label={lateOnly() ? "Show everyone" : "Show only late"} onPress={() => lateOnly.set(!lateOnly())} />}
    >
      <If when={rows().length === 0}>
        <Empty text={lateOnly() ? "Nobody is late." : "No taxpayer has forms on the calendar yet."} />
      </If>
      <Each of={rows()} key={(t) => t.tin_root}>
        {(t) => (
          <Row
            title={t().name}
            onPress={() => openApp("deadlines", t().tin_root)}
            right={
              <View class="flex-row gap-1.5">
                <Chip text={`${t().done} done`} tone="ok" />
                <If when={t().due_soon > 0}><Chip text={`${t().due_soon} due soon`} tone="warn" /></If>
                <If when={t().late > 0}><Chip text={`${t().late} late`} tone="danger" /></If>
              </View>
            }
          />
        )}
      </Each>
    </Card>
  );
}

// ------------------------------------------------------------ branches

function Branches() {
  const s = () => snap() || {};
  const branches = () => (s().branches || []) as any[];
  return (
    <Card title={s().name || "Branches"} sub={`${branches().length} ${branches().length === 1 ? "office" : "offices"}${s().rdo ? ` · head office RDO ${s().rdo}` : ""}`}
      foot={[<Btn label="Open profile" onPress={() => openApp("tax-profile", P.ticket)} />, <Btn label="Add a branch" onPress={() => openApp("tax-profile", P.ticket)} />]}>
      <Each of={branches()} key={(b) => b.code}>
        {(b) => (
          <Row
            title={`${b().code} · ${b().head_office ? "Head office" : b().name || "Branch"}`}
            sub={[b().rdo ? `RDO ${b().rdo}` : "", b().address].filter(Boolean).join(" · ")}
            right={
              <View class="flex-row gap-2 items-center">
                <If when={Boolean(b().head_office)}><Chip text="Head office" tone="info" /></If>
                <Btn label="Open" onPress={() => openApp("tax-profile", b().tin)} />
              </View>
            }
          />
        )}
      </Each>
    </Card>
  );
}

// ------------------------------------------------------------ quickadd

const TYPES = ["Individual", "Corporation", "Partnership", "Cooperative", "Estate", "Trust"];

function QuickAdd() {
  const s = () => snap() || {};
  const name = signal(String(snap()?.name || ""));
  const kind = signal(String(snap()?.type || "Individual"));
  const rdo = signal(String(snap()?.rdo || ""));
  const saved = signal(false);
  const save = () =>
    act(() => rox.call("bir_card_add_taxpayer", { ticket: s().ticket, name: name().trim(), type: kind().toLowerCase(), ...(rdo().trim() ? { rdo: rdo().trim() } : {}) } as any), `${name().trim()} is saved in BIR.`).then(
      () => !failure() && saved.set(true),
    );
  if (s().exists)
    return (
      <Card title="Already in BIR" sub={String(s().name || "")} foot={<Btn label="Open profile" primary onPress={() => openApp("tax-profile", s().tin_root)} />}>
        <Text class={`text-sm ${K.fg}`}>That TIN is saved already, so nothing was added.</Text>
      </Card>
    );
  return (
    <Card
      title="New taxpayer"
      sub={s().ticket ? `TIN ending ${s().tin_end || ""} · nothing is saved until you press Save` : "No TIN given: add them in the full form"}
      foot={
        saved()
          ? [<Btn label="Open profile" primary onPress={() => openApp("tax-profile", s().ticket)} />]
          : [
              <Btn label="Save taxpayer" primary disabled={!s().ticket || !name().trim()} onPress={save} />,
              <Btn label="Full form" onPress={() => openApp("register")} />,
            ]
      }
    >
      <View class="flex-row flex-wrap gap-2">
        <Field label="Registered name" value={name()} onChange={(v) => name.set(v)} width={260} />
        <Field label="RDO" value={rdo()} onChange={(v) => rdo.set(v)} placeholder="040" width={80} />
      </View>
      <View class="flex-row flex-wrap gap-1.5 pt-1">
        <Each of={TYPES}>
          {(t) => (
            <View onPress={() => kind.set(t())} focusable accessibilityRole="button" accessibilityLabel={t()}
              class={`rounded-full px-2.5 py-0.5 ${kind() === t() ? K.accentBg : "bg-black/5 dark:bg-white/10"}`}>
              <Text class={`text-xs ${kind() === t() ? "text-white dark:text-white" : K.fg}`}>{t()}</Text>
            </View>
          )}
        </Each>
      </View>
      <If when={Boolean(s().rdo_office)}><Text class={`text-xs ${K.fg2}`}>{`RDO ${s().rdo}: ${s().rdo_office}`}</Text></If>
    </Card>
  );
}

// ------------------------------------------------------------ streak

function Streak() {
  const s = () => snap() || {};
  const rows = () => (s().taxpayers || []) as any[];
  const tone = (rate: number): Tone => (rate >= 90 ? "ok" : rate >= 70 ? "warn" : "danger");
  const fill = (rate: number) => (rate >= 90 ? "bg-[#2e7d32] dark:bg-[#81c784]" : rate >= 70 ? "bg-[#b26a00] dark:bg-[#ffb74d]" : "bg-[#c62828] dark:bg-[#ff6b6b]");
  return (
    <Card title={`On time · ${s().year || ""}`} sub="Returns due so far this year, filed by their due date">
      <If when={rows().length === 0}><Empty text="No returns were due yet this year." /></If>
      <Each of={rows()} key={(t) => t.tin_root}>
        {(t) => (
          <Row
            title={t().name}
            sub={`${t().on_time} of ${t().due} on time${t().late ? ` · ${t().late} late` : ""}`}
            onPress={() => openApp("deadlines", t().tin_root)}
            right={
              <View class="flex-row items-center gap-2">
                <View class="rounded-full bg-black/10 dark:bg-white/10" style={{ width: 90, height: 6 }}>
                  <View class={`rounded-full ${fill(t().rate ?? 0)}`} style={{ width: Math.max(4, Math.round(((t().rate ?? 0) / 100) * 90)), height: 6 }} />
                </View>
                <Chip text={t().rate == null ? "—" : `${t().rate}%`} tone={t().rate == null ? "plain" : tone(t().rate)} />
              </View>
            }
          />
        )}
      </Each>
    </Card>
  );
}

export default function PeopleCard() {
  if (P.card === "pick") return <Pick />;
  if (P.card === "board") return <Board />;
  if (P.card === "branches") return <Branches />;
  if (P.card === "quickadd") return <QuickAdd />;
  if (P.card === "streak") return <Streak />;
  return <Taxpayer />;
}
