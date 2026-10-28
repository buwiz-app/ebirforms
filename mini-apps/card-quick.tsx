// BIR's short answer cards (spec 9.8): one decision each, so the app is a
// short card. approve (allow the assistant one send), match (a payment
// email matched to its return), markpaid (mark a return paid), confirm
// (BIR's confirmation email), signin (signed out of Buwiz), shift (a
// deadline and why it moved; move it or put it back) and remind (reminder
// days). Every button that changes something runs only when the user
// presses it (`rox.call`, origin app); the assistant can't press them.

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

// ------------------------------------------------------------ match

function Match() {
  const s = () => snap() || {};
  // A signed call carries whole numbers only: the pesos go as text.
  const record = () =>
    Object.fromEntries(Object.entries((s().record || {}) as Record<string, unknown>).filter(([, v]) => v != null).map(([k, v]) => [k, typeof v === "number" ? String(v) : v]));
  const state = signal<"" | "recorded" | "already" | "no">(s().recorded ? "already" : "");
  const rec = () => record() as Record<string, string>;
  // "Not this return", kept per payment (a storage key allows [A-Za-z0-9._:-])
  const kept = () => `match:${rec().form || ""}:${rec().period || ""}:${rec().channel || ""}:${rec().reference || ""}`.replace(/[^A-Za-z0-9._:-]/g, "-").slice(0, 128);
  // a card made before the payment was recorded learns it when it opens
  (async () => {
    try {
      if ((await rox.storage?.get(kept())) === "no") return state.set("no");
    } catch {}
    if (state() !== "" || !rec().reference) return;
    try {
      const listed = answerOf(await rox.call("bir_payments_list", { tin: rec().tin, form: rec().form, period: rec().period }));
      const same = (p: any) =>
        String(p.reference || "").toLowerCase() === String(rec().reference).toLowerCase() && String(p.channel || "").toLowerCase() === String(rec().channel || "").toLowerCase();
      if (state() === "" && ((listed?.payments || []) as any[]).some(same)) state.set("already");
    } catch {}
  })();
  const notThis = () => {
    state.set("no");
    rox.storage?.set(kept(), "no").catch(() => {});
  };
  const yes = () =>
    act(async () => {
      const r = answerOf(await rox.call("bir_record_payment", record() as any));
      state.set(r?.already ? "already" : "recorded");
    });
  const said = { recorded: "Recorded on the return.", already: "Already recorded on the return.", no: "Left alone. Tell the assistant which return it pays." };
  // once recorded, BIR's receipt page in Rox's document view
  const viewReceipt = () => act(() => rox.preview({ tool: "bir_payment_receipt", args: { tin: rec().tin, form: rec().form, period: rec().period } }));
  const paid = () => (state() === "recorded" || state() === "already") && Boolean(rox.preview);
  return (
    <Card title={`${s().channel || "Payment"} · ${peso(s().total ?? s().amount)}`} sub={`${s().form || ""} · ${s().period || ""}${s().reference ? ` · ref ${s().reference}` : ""}`}
      foot={state() === "" ? [<Btn label="Yes, mark paid" primary onPress={yes} />, <Btn label="Not this return" onPress={notThis} />] : paid() ? [<Btn label="View receipt" onPress={viewReceipt} />] : undefined}>
      <Row title="Paid on" sub={String(s().paid_at || "").replace("T", " ").slice(0, 16)} />
      <Row title="Tax and fee" sub={`${peso(s().amount)} tax · ${peso(s().fee)} fee`} />
      <If when={state() !== ""}>
        <Text class={`text-sm ${state() === "no" ? K.fg2 : K.ok}`}>{said[state() as keyof typeof said] || ""}</Text>
      </If>
    </Card>
  );
}

// ------------------------------------------------------------ markpaid

function MarkPaid() {
  const s = () => snap() || {};
  const ret = () => ({ tin: String(s().tin || ""), form: String(s().form || ""), period: String(s().period || "") });
  const d = { paid_at: signal(today()), amount: signal(""), channel: signal(""), reference: signal("") };
  const saved = signal(false);
  const save = () =>
    act(() => pay(ret(), { paid_at: d.paid_at(), amount: d.amount(), channel: d.channel(), reference: d.reference() }), "Payment recorded.").then(
      () => !failure() && saved.set(true),
    );
  return (
    <Card title={`Mark ${ret().form} ${ret().period} paid`} sub="Nothing is recorded until you press Save."
      foot={saved() ? undefined : [<Btn label="Save" primary onPress={save} />, <Btn label="Attach receipt" onPress={() => act(() => attachProof(ret()), "Receipt attached.")} />]}>
      <View class="flex-row flex-wrap gap-2">
        <Field label="Paid on" value={d.paid_at()} onChange={(v) => d.paid_at.set(v)} width={120} />
        <Field label="Amount (₱)" value={d.amount()} onChange={(v) => d.amount.set(v)} placeholder="1,629.94" width={120} />
        <Field label="Paid through" value={d.channel()} onChange={(v) => d.channel.set(v)} placeholder="MyEG, GCash, a bank" width={150} />
        <Field label="Reference" value={d.reference()} onChange={(v) => d.reference.set(v)} placeholder="optional" width={140} />
      </View>
      <Text class={`text-xs ${K.fg3}`}>Leave the amount and channel empty to mark it paid without details.</Text>
    </Card>
  );
}

// ------------------------------------------------------------ confirm

function Confirm() {
  const s = () => snap() || {};
  // BIR's file name starts with the TIN, so the card shows when BIR
  // received the return, never the file name (spec 6.6).
  return (
    <Card title={s().confirmed ? "Confirmed by BIR" : "Not confirmed yet"} sub={`${s().form || ""} · ${s().period || ""}${s().taxpayer ? ` · ${s().taxpayer}` : ""}`}
      foot={<If when={Boolean(s().token)}><Btn label="Open the return" onPress={() => openApp("forms", ticketOf(s().token))} /></If>}>
      <Row title="Status" sub={String(s().status || "")} right={<Chip text={s().confirmed ? "Confirmed" : "Waiting"} tone={s().confirmed ? "ok" : "warn"} />} />
      <If when={Boolean(s().received_at)}><Text class={`text-sm ${K.fg2}`}>{`BIR confirmation received ${String(s().received_at || "").slice(0, 16)}`}</Text></If>
      <If when={Boolean(s().reason)}><Text class={`text-sm ${K.fg2}`}>{String(s().reason || "")}</Text></If>
    </Card>
  );
}

// ------------------------------------------------------------ signin

function SignIn() {
  const s = () => snap() || {};
  const checked = signal<"" | "in" | "out">("");
  const again = () =>
    act(async () => {
      const status = answerOf(await rox.call("bir_status", {}));
      checked.set(status?.signed_in ? "in" : "out");
    });
  return (
    <Card title="Sign in to Buwiz" sub="BIR needs your Buwiz account for this."
      foot={[<Btn label="Sign in" primary onPress={() => openApp("register")} />, <Btn label="Try again" onPress={again} />]}>
      <Text class={`text-sm ${K.fg}`}>
        {checked() === "in"
          ? "You're signed in now. Ask again and BIR will answer."
          : checked() === "out"
            ? "Still signed out. Press Sign in, approve this computer in your browser, then try again."
            : String(s().message || "You're signed out of Buwiz, so BIR can't read this computer's taxpayers. Sign in to continue.")}
      </Text>
    </Card>
  );
}

// ------------------------------------------------------------ shift

const WEEKDAY = (day: string) => {
  const [y, m, d] = String(day || "").split("-").map(Number);
  return y ? ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][new Date(y, m - 1, d).getDay()] : "";
};
const longDate = (day: unknown) => `${WEEKDAY(String(day))}, ${shortDate(day)}`;

function Shift() {
  const s = () => snap() || {};
  const moved = () => s().official !== s().applies;
  const editing = signal(false);
  const to = signal("");
  const reason = signal("");
  const args = () => ({ form: s().form, period: s().period });
  const restore = () =>
    act(async () => {
      await rox.call("bir_deadline_override_remove", args() as any);
      snap.set(answerOf(await rox.call("bir_deadline", { ...args(), ...(P.args?.tin ? { tin: P.args.tin } : {}) } as any)));
    }, "Put back. Every taxpayer's calendar follows.");
  const save = () =>
    act(async () => {
      await rox.call("bir_deadline_override_set", { ...args(), new_deadline: to().trim(), ...(reason().trim() ? { reason: reason().trim() } : {}) } as any);
      editing.set(false);
      snap.set(answerOf(await rox.call("bir_deadline", { ...args(), ...(P.args?.tin ? { tin: P.args.tin } : {}) } as any)));
    }, "Moved for every taxpayer.");
  const tone: Record<string, Tone> = { weekend: "info", holiday: "info", moved: "warn", normal: "ok" };
  const word: Record<string, string> = { weekend: "weekend rule", holiday: "holiday", moved: "moved", normal: "as BIR's rule" };
  return (
    <Card
      title={`${s().label || "Deadline"}`}
      sub={s().reason || "Due on the date BIR's rule gives."}
      foot={
        editing()
          ? [<Btn label="Save the new date" primary onPress={save} disabled={!/^\d{4}-\d{2}-\d{2}$/.test(to().trim())} />, <Btn label="Cancel" onPress={() => editing.set(false)} />]
          : s().saved_move
            ? [<Btn label={`Put back ${shortDate(s().official)}`} onPress={restore} />, <Btn label="Calendar" onPress={() => openApp("calendar", P.args?.tin as string | undefined)} />]
            : [<Btn label="Move it" onPress={() => { to.set(String(s().applies || "")); editing.set(true); }} />, <Btn label="Calendar" onPress={() => openApp("calendar", P.args?.tin as string | undefined)} />]
      }
    >
      <Fact label="BIR's rule" value={moved() ? <Text class={`text-sm line-through ${K.fg2}`}>{longDate(s().official)}</Text> : longDate(s().official)} />
      <Fact label="Applies" value={<View class="flex-row items-center gap-2"><Text class={`text-sm font-semibold ${K.fg}`}>{longDate(s().applies)}</Text><Chip text={word[s().kind] || String(s().kind || "")} tone={tone[s().kind] || "plain"} /></View>} />
      <If when={editing()}>
        <View class="flex-row gap-2 pt-1">
          <Field label="New due date" value={to()} onChange={(v) => to.set(v)} placeholder="2026-11-03" width={130} />
          <Field label="Why (an advisory)" value={reason()} onChange={(v) => reason.set(v)} placeholder="RMC 99-2026" width={200} />
        </View>
      </If>
    </Card>
  );
}

// ------------------------------------------------------------ remind

function Remind() {
  const s = () => snap() || {};
  const days = signal<number[]>(((snap()?.reminder_days || []) as number[]).slice());
  const saved = signal(true);
  const toggle = (d: number) => {
    days.set(days().includes(d) ? days().filter((x) => x !== d) : [...days(), d].sort((a, b) => b - a));
    saved.set(false);
  };
  const save = () =>
    act(async () => {
      snap.set(answerOf(await rox.call("bir_calendar_set", { tin: s().tin, reminder_days: days() } as any)));
      saved.set(true);
    }, "Saved.");
  const on = () => days().length > 0;
  return (
    <Card
      title={`Reminders · ${s().taxpayer || ""}`}
      sub={on() ? `${days().map((d) => (d === 0 ? "on the day" : `${d} day${d === 1 ? "" : "s"} before`)).join(", ")} every deadline` : "Off for this taxpayer"}
      foot={[<Btn label="Save" primary disabled={saved()} onPress={save} />, <Btn label="Calendar" onPress={() => openApp("calendar", s().tin)} />]}
    >
      <View class="flex-row flex-wrap gap-2">
        <Each of={[14, 7, 3, 1, 0]}>
          {(d) => (
            <View onPress={() => toggle(d())} focusable accessibilityRole="button" accessibilityLabel={`${d()} days before`}
              class={`rounded-full px-3 py-1 ${days().includes(d()) ? K.accentBg : "bg-black/5 dark:bg-white/10"}`}>
              <Text class={`text-sm ${days().includes(d()) ? "text-white dark:text-white" : K.fg}`}>{d() === 0 ? "On the day" : `${d()} day${d() === 1 ? "" : "s"}`}</Text>
            </View>
          )}
        </Each>
      </View>
      <Text class={`text-xs ${K.fg3}`}>{`For the ${((s().forms || []) as any[]).filter((f) => f.enabled).length} forms on the calendar; Google Calendar gets them on the next sync.`}</Text>
    </Card>
  );
}

export default function QuickCard() {
  if (P.card === "approve") return <Approve />;
  if (P.card === "match") return <Match />;
  if (P.card === "markpaid") return <MarkPaid />;
  if (P.card === "confirm") return <Confirm />;
  if (P.card === "shift") return <Shift />;
  if (P.card === "remind") return <Remind />;
  return <SignIn />;
}
