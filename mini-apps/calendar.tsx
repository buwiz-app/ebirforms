// BIR Calendar mini app: a port of bir-shell's Calendar page
// (app/shell/calendar_page.js, rows from home.js `dueRow`). Every deadline
// for the forms the taxpayers file, and every as-needed return that holds
// its trigger date, grouped by month, from the same host function the page
// reads (`deadlines_between`: bir-core's calendar rules over each branch's
// yearly forms, the stored deadline overrides and each profile's own
// applied). Holidays move deadlines only in the years the app ships a
// holiday list for (2026 so far); a later year's deadlines move off weekends
// only.
//
// Opened for one taxpayer (`roxProps.tin`) it shows that branch's deadlines
// or, as the page's "Whose deadlines" switch does, all of them (held to the
// taxpayer's TIN root). A row opens its return in the BIR Forms mini app.
//
// Added to the page: this year's schedule (the page shows the next twelve
// months, kept as the other range), and deadline overrides. A deadline the
// user moves is a rule for every taxpayer (`deadline_override_set`, the
// same storage the desktop's admin calendar writes and the Google Calendar
// sync reads); a moved row says so and can be put back.

import { signal, computed, resource, Text, View, Scroll, Input, Overlay } from "nsh";

/** An `<Input>`'s text from its `onChange`: `{ value }` since nsh's events
 *  became objects (#558), the text itself before. */
const typed = (e: { value: string } | string): string => (typeof e === "string" ? e : e.value);

// =====================================================================
// HOST CALLS: every call goes through `bir` (the daemon's `bir_call`
// bridge). Names and shapes follow dashboard_module.rs DECLARATIONS.
// =====================================================================

type RoxProps = { server?: string; tin?: string; scope?: string };
const roxProps = (): RoxProps => (globalThis as any).roxProps || {};
const digits = (v: unknown) => String(v ?? "").replace(/\D/g, "");
/** The branch Rox opened the app for, and the TIN root every call is held to. */
// a 9-digit root is the head office: rows carry the 14-digit branch TIN
const TIN = ((t) => (t.length === 9 ? `${t}00000` : t))(digits(roxProps().tin));
const SCOPE: string | undefined = roxProps().scope || (TIN ? TIN.slice(0, 9) : undefined);

async function bir<T = any>(fn: string, ...args: unknown[]): Promise<T> {
  const p = roxProps();
  const r = await fetch("http://127.0.0.1:8787/mcp/call", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ server: p.server || "bir", tool: "bir_call", args: { fn, args, scope: SCOPE } }),
  }).then((x) => x.json());
  if (!r.ok) throw new Error(r.error || "BIR call failed");
  return r.result as T;
}

/** Opens a `rox://app/…` link through Rox (nsh has no open-URL API). */
async function openRox(url: string) {
  const r = await fetch("http://127.0.0.1:8787/mcp/open", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ url }),
  });
  if (!r.ok) throw new Error(`Rox could not open the return (${r.status})`);
}

interface DeadlineRow {
  tin: string;
  tin_root: string;
  branch_code: string;
  branch_name: string;
  taxpayer_name: string;
  branch_count: number;
  form_code: string;
  form_name: string;
  frequency: "Monthly" | "Quarterly" | "Annual" | "As needed";
  taxable_year: number;
  period: number;
  period_label: string;
  due_date: string;
  status: string;
  draft_id: number | null;
  return_key: number | null;
  original_due_date: string;
  deadline_status: "normal" | "weekend" | "holiday" | "non_working" | "moved";
  moved_reason: string | null;
  period_key: string | null;
}
interface OverrideEntry {
  id: string;
  kind: "holiday" | "moved";
  title: string;
  reason: string;
  forms: string[];
  original_deadline: string;
  new_deadline: string;
  periods: { form: string; period: string; label: string }[];
}
interface OverrideList { year: number | null; overrides: OverrideEntry[]; damaged: number; unreadable: boolean }
interface MoveArgs { form: string; period: string; new_deadline: string; reason?: string }

const api = {
  deadlines: (from: string, to: string) => bir<DeadlineRow[]>("deadlines_between", from, to),
  overrides: (year: number) => bir<OverrideList>("deadline_overrides", { year }),
  move: (args: MoveArgs) => bir<{ label: string; due: string | null }>("deadline_override_set", args),
  restore: (args: { id?: string; form?: string; period?: string }) => bir<{ label?: string; due?: string | null }>("deadline_override_remove", args),
};

// =====================================================================
// Helpers
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
  warning: "text-[#b26a00] dark:text-[#ffb74d]",
  success: "text-[#2e7d32] dark:text-[#81c784]",
  accent: "text-[#1680f3] dark:text-[#1884fc]",
  accentBg: "bg-[#1680f3] dark:bg-[#1884fc]",
  accentSoft: "bg-[#1680f31f] dark:bg-[#1884fc33]",
  dangerSoft: "bg-[#c628281a] dark:bg-[#ff6b6b26]",
  warningSoft: "bg-[#b26a001f] dark:bg-[#ffb74d26]",
  successSoft: "bg-[#2e7d321f] dark:bg-[#66bb6a33]",
  input: "placeholder:text-[#8e8e93] h-8 px-2 rounded-md border border-black/10 dark:border-white/10 bg-white dark:bg-[#161616] text-sm text-[#1c1c1c] dark:text-[#f5f5f5]",
};

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_LONG = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

const pad = (n: number) => String(n).padStart(2, "0");
const isoDate = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
function addDays(d: Date, n: number) {
  const out = new Date(d);
  out.setDate(out.getDate() + n);
  return out;
}
/** Days from today to an ISO date (store.js `daysUntil`). */
function daysUntil(iso: string) {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const [y, m, d] = iso.split("-").map(Number);
  return Math.round((new Date(y, m - 1, d).getTime() - today.getTime()) / 86400000);
}
/** "Oct 25, 2026". */
function shortDate(iso: string) {
  const [y, m, d] = iso.split("-").map(Number);
  return `${MONTHS[m - 1]} ${d}, ${y}`;
}
function errText(e: unknown) {
  return e instanceof Error ? e.message : String(e);
}

/** home.js `statusKind`. */
function statusKind(status: string) {
  if (status === "Draft") return "draft";
  if (status === "Queued") return "queued";
  if (status === "Submitted" || status === "Confirmed" || status === "Paid") return "filed";
  return "not_started";
}
/** Who a row belongs to: the taxpayer, plus the branch when it has several (home.js `whoLine`). */
const whoLine = (d: DeadlineRow) => (d.branch_count > 1 ? `${d.taxpayer_name} · ${d.branch_name}` : d.taxpayer_name);

/** ui.js `StatusChip`: label and tone. */
function chip(kind: string, days = 0): [string, string, string] {
  switch (kind) {
    case "draft":
      return ["Draft", C.accentSoft, C.accent];
    case "queued":
      return ["Queued", C.accentSoft, C.accent];
    case "filed":
      return ["Filed", C.successSoft, C.success];
    case "due_soon":
      return [days === 0 ? "Due today" : `In ${days} day${days === 1 ? "" : "s"}`, C.warningSoft, C.warning];
    case "overdue":
      return [`${Math.abs(days)} day${Math.abs(days) === 1 ? "" : "s"} overdue`, C.dangerSoft, C.danger];
    default:
      return ["Not started", C.inset, C.fg2];
  }
}

// =====================================================================
// State
// =====================================================================

const now = new Date();
/** "year": one calendar year's deadlines; "upcoming": the page's next twelve months. */
const range = signal<"year" | "upcoming">("year");
const year = signal(now.getFullYear());
/** The page's "Whose deadlines": the branch Rox opened, or all. */
const whose = signal<"mine" | "all">(TIN ? "mine" : "all");
const busy = signal(false);
const message = signal<{ ok: boolean; text: string } | null>(null);
const ver = signal(0);
/** The "Move a deadline" dialog, prefilled from a row or blank. */
const editor = signal<{ form: string; period: string; label: string; due: string } | null>(null);
const editForm = signal("");
const editPeriod = signal("");
const editDate = signal("");
const editReason = signal("");
const editError = signal("");

const window_ = computed(() => {
  if (range() === "upcoming") return { from: isoDate(now), to: isoDate(addDays(now, 366)) };
  return { from: `${year()}-01-01`, to: `${year()}-12-31` };
});

const deadlines = resource(
  () => ({ from: window_().from, to: window_().to, v: ver() }),
  (s) => api.deadlines(s.from, s.to),
);
const overrides = resource(
  () => ({ year: range() === "upcoming" ? now.getFullYear() : year(), v: ver() }),
  (s) => api.overrides(s.year),
);

const rows = computed(() => {
  const all = deadlines() || [];
  return whose() === "mine" && TIN ? all.filter((d) => digits(d.tin) === TIN) : all;
});
/** Rows by month, in date order (calendar_page.js groups). */
const groups = computed(() => {
  const out: { label: string; rows: DeadlineRow[] }[] = [];
  for (const d of rows()) {
    const [y, m] = d.due_date.split("-").map(Number);
    const label = `${MONTHS_LONG[m - 1]} ${y}`;
    const last = out[out.length - 1];
    if (last && last.label === label) last.rows.push(d);
    else out.push({ label, rows: [d] });
  }
  return out;
});
const mine = computed(() => (deadlines() || []).find((d) => digits(d.tin) === TIN) || null);
const moved = computed(() => (overrides()?.overrides || []).filter((o) => o.kind === "moved"));
const holidays = computed(() => (overrides()?.overrides || []).filter((o) => o.kind === "holiday").length);

function say(ok: boolean, text: string) {
  message.set({ ok, text });
  setTimeout(() => {
    if (message()?.text === text) message.set(null);
  }, 5000);
}

/** Open the row's return in the Forms app (the page's `app.openForm`). */
async function open(d: DeadlineRow) {
  const props = { tin: d.tin, scope: d.tin_root, form_code: d.form_code, year: d.taxable_year, period: d.period, key: d.return_key };
  try {
    await openRox(`rox://app/${roxProps().server || "bir"}/forms?props=${encodeURIComponent(JSON.stringify(props))}`);
  } catch (err) {
    say(false, errText(err));
  }
}

function startMove(d: DeadlineRow | null) {
  editForm.set(d ? d.form_code : "");
  editPeriod.set(d?.period_key || "");
  editDate.set(d ? d.due_date : "");
  editReason.set(d?.moved_reason || "");
  editError.set("");
  editor.set(d ? { form: d.form_code, period: d.period_key || "", label: `${d.form_code} · ${d.period_label}`, due: d.due_date } : { form: "", period: "", label: "", due: "" });
}

async function saveMove() {
  const form = editForm().trim();
  const period = editPeriod().trim();
  const date = editDate().trim();
  if (!form || !period) return editError.set("Name the form and its period, like 2551Q and 2026-Q3.");
  if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) return editError.set("Type the new due date as YYYY-MM-DD, like 2026-11-03.");
  busy.set(true);
  try {
    const reason = editReason().trim();
    const r = await api.move({ form, period, new_deadline: date, ...(reason ? { reason } : {}) });
    editor.set(null);
    ver.set(ver() + 1);
    say(true, `${form} ${r.label} is now due ${r.due ? shortDate(r.due) : date} for every taxpayer.`);
  } catch (err) {
    editError.set(errText(err));
  }
  busy.set(false);
}

async function restore(args: { id?: string; form?: string; period?: string }, what: string) {
  if (busy()) return;
  busy.set(true);
  try {
    const r = await api.restore(args);
    ver.set(ver() + 1);
    say(true, r.due ? `${what} is back to BIR's date: ${shortDate(r.due)}.` : `${what} is back to BIR's date.`);
  } catch (err) {
    say(false, errText(err));
  }
  busy.set(false);
}

// =====================================================================
// Components
// =====================================================================

function Btn(p: { label: string; onPress: () => void; variant?: "primary" | "secondary" | "ghost"; disabled?: boolean }) {
  const v = () => p.variant || "secondary";
  return (
    <View
      class={`rounded-md items-center justify-center px-3 py-1.5 ${v() === "primary" ? C.accentBg : v() === "ghost" ? "" : C.inset} ${p.disabled ? "opacity-40" : ""}`}
      activeStyle={{ opacity: 0.7 }}
      onPress={() => !p.disabled && p.onPress()}
    >
      <Text class={`text-sm font-semibold ${v() === "primary" ? "text-white dark:text-white" : v() === "ghost" ? C.accent : C.fg}`}>{p.label}</Text>
    </View>
  );
}

function Segmented<T extends string>(p: { options: { value: T; label: string }[]; value: T; onChange: (v: T) => void }) {
  return (
    <View class={`flex-row gap-0.5 p-0.5 rounded-md self-start ${C.inset}`}>
      <Each of={p.options}>
        {(o) => (
          <View
            onPress={() => p.onChange(o().value)}
            activeStyle={{ opacity: 0.7 }}
            class={`px-3 py-1 rounded-md ${o().value === p.value ? `${C.pane} border ${C.line}` : ""}`}
          >
            <Text class={`text-xs font-medium ${o().value === p.value ? C.fg : C.fg2}`}>{o().label}</Text>
          </View>
        )}
      </Each>
    </View>
  );
}

function Chip(p: { kind: string; days?: number }) {
  const c = () => chip(p.kind, p.days ?? 0);
  return (
    <View class={`rounded-full px-2 py-0.5 self-start ${c()[1]}`}>
      <Text class={`text-xs font-medium ${c()[2]}`}>{c()[0]}</Text>
    </View>
  );
}

/** home.js `dueRow`: date box, code, "<code> · <period>" over the taxpayer, when, status; plus a moved deadline's note and actions. */
function DueRow(p: { d: DeadlineRow }) {
  const d = () => p.d;
  const n = () => daysUntil(d().due_date);
  const isOpen = () => d().status === "Not started" || d().status === "Draft";
  const day = () => d().due_date.split("-").map(Number);
  const movedNote = () =>
    `Moved from ${shortDate(d().original_due_date)}${d().moved_reason ? ` · ${d().moved_reason}` : ""}`;
  return (
    <View class={`flex-row items-center gap-3 px-3 py-2.5 border-t ${C.line}`} activeStyle={{ opacity: 0.6 }} onPress={() => open(d())}>
      <View class="items-center" style={{ width: 44 }}>
        <Text class={`text-xs font-semibold ${C.fg3}`}>{MONTHS[day()[1] - 1].toUpperCase()}</Text>
        <Text class={`text-lg font-semibold ${C.fg}`}>{String(day()[2])}</Text>
      </View>
      <View style={{ width: 68 }}>
        <Text class={`text-xs font-semibold ${C.fg2}`}>{d().form_code}</Text>
      </View>
      <View style={{ flex: 1 }} class="gap-0.5">
        <Text class={`text-sm font-semibold ${C.fg}`} numberOfLines={1}>{`${d().form_code} · ${d().period_label}`}</Text>
        <Text class={`text-xs ${C.fg2}`} numberOfLines={1}>{whoLine(d())}</Text>
        <If when={d().deadline_status === "moved"}>
          <Text class={`text-xs ${C.warning}`} numberOfLines={1}>{movedNote()}</Text>
        </If>
      </View>
      <View style={{ width: 110 }}>
        <If
          when={isOpen() && n() < 0}
          else={
            <If when={isOpen() && n() <= 14} else={<Text class={`text-xs ${C.fg2}`}>{n() === 0 ? "Today" : n() < 0 ? `${-n()} days ago` : `In ${n()} days`}</Text>}>
              <Chip kind="due_soon" days={n()} />
            </If>
          }
        >
          <Chip kind="overdue" days={n()} />
        </If>
      </View>
      <View style={{ width: 88 }} class="items-end">
        <Chip kind={statusKind(d().status)} />
      </View>
      <View style={{ width: 132 }} class="flex-row justify-end gap-1">
        <If when={Boolean(d().period_key)}>
          <Btn label="Move" variant="ghost" disabled={busy()} onPress={() => startMove(d())} />
        </If>
        <If when={d().deadline_status === "moved" && Boolean(d().period_key)}>
          <Btn label="Restore" variant="ghost" disabled={busy()} onPress={() => restore({ form: d().form_code, period: d().period_key || "" }, `${d().form_code} · ${d().period_label}`)} />
        </If>
      </View>
    </View>
  );
}

/** The deadlines moved for everyone, each with its periods and a Restore. */
function MovedDeadlines() {
  return (
    <View class="gap-2">
      <View class="flex-row items-center gap-3">
        <View style={{ flex: 1 }} class="gap-0.5">
          <Text class={`text-sm font-semibold ${C.fg}`}>Moved deadlines</Text>
          <Text class={`text-xs ${C.fg2}`}>
            {`A moved deadline applies to every taxpayer: the calendar, Google Calendar and the penalties of every return of that form and period follow it.${holidays() ? ` ${holidays()} official holidays also move the deadlines that fall on them.` : ""}`}
          </Text>
        </View>
        <Btn label="Move a deadline" onPress={() => startMove(null)} />
      </View>
      <If when={Boolean(overrides()?.unreadable) || (overrides()?.damaged || 0) > 0}>
        <Text class={`text-xs ${C.danger}`}>
          {overrides()?.unreadable ? "The stored deadline overrides cannot be read; fix them in the desktop app." : `${overrides()?.damaged} stored deadline override(s) cannot be read and are kept as stored.`}
        </Text>
      </If>
      <View class={`rounded-xl ${C.card}`}>
        <If when={moved().length === 0}>
          <View class="px-3 py-3">
            <Text class={`text-sm ${C.fg2}`}>No deadline is moved this year.</Text>
          </View>
        </If>
        <Each of={moved()} key={(o) => o.id}>
          {(o) => (
            <View class={`flex-row items-center gap-3 px-3 py-2.5 border-t ${C.line}`}>
              <View style={{ flex: 1 }} class="gap-0.5">
                <Text class={`text-sm font-semibold ${C.fg}`} numberOfLines={1}>
                  {o().periods.length ? o().periods.map((x) => `${x.form} · ${x.label}`).join(", ") : `${o().forms.join(", ")} · ${o().title}`}
                </Text>
                <Text class={`text-xs ${C.fg2}`} numberOfLines={1}>{`${shortDate(o().original_deadline)} → ${shortDate(o().new_deadline)} · ${o().reason}`}</Text>
              </View>
              <Btn label="Restore" variant="ghost" disabled={busy()} onPress={() => restore({ id: o().id }, o().title)} />
            </View>
          )}
        </Each>
      </View>
    </View>
  );
}

function Field(p: { label: string; value: string; placeholder: string; onChange: (text: string) => void; readonly?: boolean }) {
  return (
    <View class="gap-1">
      <Text class={`text-xs ${C.fg3}`}>{p.label}</Text>
      <Input class={C.input} value={p.value} placeholder={p.placeholder} readonly={p.readonly} onChange={(e) => p.onChange(typed(e))} onSubmit={() => saveMove()} />
    </View>
  );
}

/** "Move a deadline": form, period, new date and reason; for every taxpayer. */
function MoveDialog() {
  const e = () => editor();
  const fromRow = () => Boolean(e()?.form);
  return (
    <If when={Boolean(e())}>
      <Overlay class="inset-0 bg-black/40" onPress={() => editor.set(null)} />
      <Overlay class="top-20 left-0 right-0 items-center" onCancel={() => editor.set(null)}>
        <View class={`rounded-xl p-4 gap-3 border ${C.line} ${C.pane}`} style={{ width: 420 }}>
          <Text class={`text-base font-semibold ${C.fg}`}>{fromRow() ? `Move ${e()?.label}` : "Move a deadline"}</Text>
          <Text class={`text-xs ${C.fg2}`}>
            {fromRow()
              ? `Due ${shortDate(e()?.due || "")} now. The new date applies to every taxpayer who files ${e()?.form}.`
              : "The new date applies to every taxpayer who files the form."}
          </Text>
          <View class="flex-row gap-2">
            <View style={{ flex: 1 }}>
              <Field label="Form" value={editForm()} placeholder="2551Q" readonly={fromRow()} onChange={(t) => editForm.set(t)} />
            </View>
            <View style={{ flex: 1 }}>
              <Field label="Period" value={editPeriod()} placeholder="2026-Q3" readonly={fromRow()} onChange={(t) => editPeriod.set(t)} />
            </View>
          </View>
          <Field label="New due date" value={editDate()} placeholder="2026-11-03" onChange={(t) => editDate.set(t)} />
          <Field label="Reason (shown on the calendar)" value={editReason()} placeholder="BIR advisory, e.g. RMC No. 99-2026" onChange={(t) => editReason.set(t)} />
          <If when={Boolean(editError())}>
            <Text class={`text-xs ${C.danger}`}>{editError()}</Text>
          </If>
          <View class="flex-row justify-end gap-2">
            <Btn label="Cancel" variant="ghost" onPress={() => editor.set(null)} />
            <Btn label={busy() ? "Moving…" : "Move deadline"} variant="primary" disabled={busy()} onPress={() => saveMove()} />
          </View>
        </View>
      </Overlay>
    </If>
  );
}

export default function R() {
  const lede = computed(
    () => `Every deadline for the forms your taxpayers file, from the ${range() === "upcoming" ? now.getFullYear() : year()} BIR tax calendar.`,
  );
  const mineLabel = computed(() => {
    const m = mine();
    return m ? (m.branch_count > 1 ? `${m.taxpayer_name} · ${m.branch_name}` : m.taxpayer_name) : "This taxpayer";
  });
  return (
    <View style={{ flex: 1 }} class={C.pane}>
      <Scroll class="flex-1" contentClass="grow">
        <View class="p-6 gap-4">
          <View class="gap-1">
            <Text class={`text-2xl font-semibold ${C.fg}`}>Calendar</Text>
            <Text class={`text-sm ${C.fg2}`}>{lede()}</Text>
          </View>
          <View class="flex-row flex-wrap items-center gap-3">
            <Segmented
              options={[
                { value: "year", label: String(year()) },
                { value: "upcoming", label: "Next 12 months" },
              ]}
              value={range()}
              onChange={(v) => range.set(v as "year" | "upcoming")}
            />
            <If when={range() === "year"}>
              <View class="flex-row items-center">
                <Btn label="‹" variant="ghost" onPress={() => year.set(year() - 1)} />
                <Btn label="›" variant="ghost" onPress={() => year.set(year() + 1)} />
              </View>
            </If>
            <If when={Boolean(TIN)}>
              <Segmented
                options={[
                  { value: "mine", label: mineLabel() },
                  { value: "all", label: "All branches" },
                ]}
                value={whose()}
                onChange={(v) => whose.set(v as "mine" | "all")}
              />
            </If>
          </View>
          <If when={Boolean(message())}>
            <View class={`px-3 py-2 rounded-lg ${message()?.ok ? C.accentSoft : C.dangerSoft}`}>
              <Text class={`text-sm ${message()?.ok ? C.fg : C.danger}`}>{message()?.text || ""}</Text>
            </View>
          </If>
          <If when={Boolean(deadlines.error())}>
            <View class={`p-3 rounded-xl gap-2 ${C.dangerSoft}`}>
              <Text class={`text-sm ${C.danger}`}>{errText(deadlines.error())}</Text>
              <View class="flex-row">
                <Btn label="Retry" onPress={() => deadlines.refetch()} />
              </View>
            </View>
          </If>
          <If when={!deadlines() && !deadlines.error()}>
            <Text class={`text-sm ${C.fg2}`}>Loading the calendar…</Text>
          </If>
          <If when={Boolean(deadlines()) && rows().length === 0}>
            <View class={`rounded-xl px-3 py-3 ${C.card}`}>
              <Text class={`text-sm ${C.fg2}`}>Deadlines show here once a taxpayer has forms for a tax year.</Text>
            </View>
          </If>
          <Each of={groups()} key={(g) => g.label}>
            {(g) => (
              <View class="gap-2">
                <Text class={`text-sm font-semibold ${C.fg}`}>{g().label}</Text>
                <View class={`rounded-xl ${C.card}`}>
                  <Each of={g().rows} key={(d) => `due-${d.tin}-${d.form_code}-${d.taxable_year}-${d.period}${d.frequency === "As needed" ? `-${d.draft_id}` : ""}`}>
                    {(d) => <DueRow d={d()} />}
                  </Each>
                </View>
              </View>
            )}
          </Each>
          <View class={`pt-4 border-t ${C.line}`}>
            <MovedDeadlines />
          </View>
        </View>
      </Scroll>
      <MoveDialog />
    </View>
  );
}
