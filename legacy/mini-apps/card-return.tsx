// BIR answer cards about one return (spec 9.8): its computation (summary),
// its filled boxes laid out like the paper form (preview: Rox has no PDF
// viewer, so this is the return's own values on a page, with Open in Forms
// for the printable one), what an amendment changes (amend), the
// attachments its form asks for (attach) and filling it from a sales sheet
// (import). Upload and Fill change something and run only when the user
// presses them (`rox.call`, origin app).

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

const ret = () => snap() || {};
const openReturn = () => openApp("forms", String(ret().key || P.ticket || ""));
const sub = () => [ret().taxpayer, ret().status ? statusChip(String(ret().status).toLowerCase() === "submitted" ? "filed" : ret().status)[0] : "Not started"].filter(Boolean).join(" · ");
/** A box's amount as pesos when it is one (BIR writes `186,000.00`). */
const amount = (value: unknown) => {
  const n = Number(String(value || "").replace(/,/g, ""));
  return /^-?[\d,]+\.\d{2}$/.test(String(value || "")) && !Number.isNaN(n) ? peso(n) : String(value || "");
};

// ------------------------------------------------------------ summary

function Summary() {
  const items = () => (ret().items || []) as any[];
  const payable = () => ret().payable;
  return (
    <Card title={`${ret().label || "Return"} · computation`} sub={sub()} foot={<Btn label="Open in Forms" onPress={openReturn} />}>
      <If when={items().length === 0}><Empty text={String(ret().note || "Nothing is filled in yet.")} /></If>
      <Each of={items().filter((it) => !payable() || it.item !== payable().item || it.label !== payable().label)}>
        {(it) => <Fact label={it().item ? `Item ${it().item}` : "Schedule"} value={<View class="flex-row justify-between gap-2"><Text class={`text-sm flex-1 ${K.fg2}`} numberOfLines={1}>{it().label}</Text><Text class={`text-sm ${K.fg}`}>{amount(it().value)}</Text></View>} />}
      </Each>
      <If when={Boolean(payable())}>
        <View class={`flex-row items-center px-2 py-1.5 rounded-lg ${K.soft}`}>
          <Text class={`text-sm font-semibold flex-1 ${K.fg}`}>{`Item ${payable()?.item} · ${payable()?.label}`}</Text>
          <Text class={`text-lg font-semibold ${K.fg}`}>{amount(payable()?.value)}</Text>
        </View>
      </If>
    </Card>
  );
}

// ------------------------------------------------------------ preview

function Preview() {
  const items = () => (ret().items || []) as any[];
  return (
    <Card title={`${ret().label || "Return"} · paper view`} sub="The filled boxes as they print, from the saved return" foot={<Btn label="Open in Forms" primary onPress={openReturn} />}>
      <View class={`rounded-md border ${K.line} bg-[#fbfaf6] dark:bg-[#202020]`}>
        <View class={`flex-row items-center gap-2 px-2 py-1.5 border-b ${K.line}`}>
          <Text class={`text-xs font-semibold ${K.fg}`}>{`BIR Form No. ${ret().form || ""}`}</Text>
          <Text class={`text-xs flex-1 ${K.fg2}`} numberOfLines={1}>{String(ret().title || "")}</Text>
          <Text class={`text-xs ${K.fg2}`}>{String(ret().period || "")}</Text>
        </View>
        <View class="px-2 py-1">
          <Text class={`text-xs ${K.fg2}`}>{`Taxpayer: ${ret().taxpayer || ""}`}</Text>
        </View>
        <If when={items().length === 0}><Empty text={String(ret().note || "Nothing is filled in yet.")} /></If>
        <View class="flex-row flex-wrap px-1 pb-1">
          <Each of={items()}>
            {(it) => (
              <View class="px-1 py-0.5" style={{ width: 254 }}>
                <Text class={`text-xs ${K.fg3}`} numberOfLines={1}>{`${it().item ? `${it().item} ` : ""}${it().label}`}</Text>
                <View class={`border ${K.line} rounded-sm px-1.5 py-0.5 ${K.pane}`}>
                  <Text class={`text-xs ${it().computed ? "font-semibold" : ""} ${K.fg}`} numberOfLines={1}>{amount(it().value)}</Text>
                </View>
              </View>
            )}
          </Each>
        </View>
      </View>
      <If when={Boolean(ret().more)}><Text class={`text-xs ${K.fg3}`}>More boxes than fit here: open it in Forms.</Text></If>
    </Card>
  );
}

// ------------------------------------------------------------ amend

function Amend() {
  const changes = () => (ret().changes || []) as any[];
  return (
    <Card title={`${ret().label || "Return"} · amendment`} sub={ret().state === "amending" ? `${changes().length} item${changes().length === 1 ? "" : "s"} change against the filed copy` : sub()}
      foot={<Btn label={ret().state === "not_started" ? "Amend in Forms" : "Open the amendment"} primary onPress={openReturn} />}>
      <If when={ret().state !== "amending"}><Text class={`text-sm ${K.fg}`}>{String(ret().note || "")}</Text></If>
      <If when={ret().state === "amending" && changes().length === 0}><Empty text="Nothing changed yet: the amended copy matches the filed one." /></If>
      <Each of={changes()}>
        {(c) => (
          <Row title={`${c().item ? `Item ${c().item} · ` : ""}${c().label}`}
            right={
              <View class="flex-row gap-2 items-center">
                <Text class={`text-sm line-through ${K.fg3}`}>{amount(c().before) || "—"}</Text>
                <Text class={`text-sm ${K.fg3}`}>→</Text>
                <Text class={`text-sm font-semibold ${K.fg}`}>{amount(c().after) || "—"}</Text>
              </View>
            } />
        )}
      </Each>
    </Card>
  );
}

// ------------------------------------------------------------ attach

const ATTACH_FILES = [".pdf", ".png", ".jpg", ".jpeg", ".heic"];

function Attach() {
  const items = () => (ret().items || []) as any[];
  const kept = signal<Record<string, string>>({});
  const upload = (it: any) =>
    act(async () => {
      const file = await uploadFile(ATTACH_FILES);
      if (!file) return;
      await rox.call("bir_card_attach", { ticket: String(ret().key || ""), attachment: it.id, upload: file.upload, name: file.name });
      kept.set({ ...kept(), [it.id]: file.name });
    });
  const has = (it: any) => kept()[it.id] || it.file;
  return (
    <Card title={`${ret().label || "Return"} · attachments`} sub={`${items().filter(has).length} of ${items().length} uploaded · each applies only when its condition does`}
      foot={<Btn label="Open in Forms" onPress={openReturn} />}>
      <If when={items().length === 0}><Empty text="The official form lists no attachments." /></If>
      <Each of={items()} key={(it) => it.id}>
        {(it) => (
          <Row title={it().label} sub={has(it()) ? `Uploaded: ${has(it())}` : it().when}
            right={
              <View class="flex-row gap-2 items-center">
                <Chip text={has(it()) ? "Uploaded" : "Missing"} tone={has(it()) ? "ok" : "warn"} />
                <Btn label={has(it()) ? "Replace" : "Upload"} onPress={() => upload(it())} />
              </View>
            } />
        )}
      </Each>
    </Card>
  );
}

// ------------------------------------------------------------ import

function Import() {
  const read = signal<any>(null);
  const filled = signal<any>(null);
  const pick = () =>
    act(async () => {
      const file = await uploadFile([".xlsx", ".csv"]);
      if (!file) return;
      filled.set(null);
      read.set(answerOf(await rox.call("bir_card_import", { ticket: String(ret().key || ""), upload: file.upload, name: file.name })));
    });
  const fill = () =>
    act(async () => {
      filled.set(answerOf(await rox.call("bir_card_import_fill", { ticket: String(ret().key || ""), import: String(read()?.import || "") })));
    }, "Filled. Check it in Forms before you send it.");
  const skipped = () => (read()?.skipped || []) as any[];
  const usable = () => Boolean(ret().box) && ret().draft !== false;
  return (
    <Card
      title={read() ? String(read().file || "Spreadsheet") : `Import into ${ret().label || "the return"}`}
      sub={read() ? `${read().rows} row${read().rows === 1 ? "" : "s"} read · ${skipped().length} skipped · ${read().period}` : usable() ? `Fills: ${ret().box}` : String(ret().note || "")}
      foot={
        filled()
          ? [<Btn label="Open in Forms" primary onPress={openReturn} />]
          : read()
            ? [<Btn label={`Fill ${ret().form} with ${read().rows} rows`} primary disabled={!read().rows} onPress={fill} />, <Btn label="Pick another file" onPress={pick} />]
            : usable()
              ? [<Btn label="Pick a sales sheet" primary onPress={pick} />]
              : [<Btn label="Open in Forms" onPress={openReturn} />]
      }
    >
      <If when={!read()}>
        <Text class={`text-sm ${K.fg2}`}>{usable() ? "An .xlsx or .csv with a date and an amount on each row. BIR reads it and shows what it skipped; nothing is filled until you press Fill." : ""}</Text>
      </If>
      <If when={Boolean(read())}>
        <Row title="Rows read" right={<Text class={`text-sm font-semibold ${K.fg}`}>{`${read()?.rows ?? 0} · ${peso(read()?.total ?? 0)}`}</Text>} />
        <Each of={skipped()}>
          {(r) => <Row title={`Row ${r().row}: ${r().why}`} sub={String(r().cell || "")} right={<Chip text="Skipped" tone="warn" />} />}
        </Each>
      </If>
    </Card>
  );
}

export default function ReturnCard() {
  if (P.card === "preview") return <Preview />;
  if (P.card === "amend") return <Amend />;
  if (P.card === "attach") return <Attach />;
  if (P.card === "import") return <Import />;
  return <Summary />;
}
