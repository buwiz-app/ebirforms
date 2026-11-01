// BIR Deadlines: the next returns due, as a small card in the chat (inline
// 480×320) or full screen. Opened for one taxpayer (`buwiz.props.ticket`) it
// lists that taxpayer's returns; without a ticket, every taxpayer's.
//
// One read tool feeds it: `ebirforms_calendar_get`, the form calendar the
// Calendar app and the assistant read (each return's due date and status).
// What it answers names taxpayers by ticket only, so the card never holds a
// TIN. A row's Open button opens that return in BIR Forms; Calendar opens
// the full schedule.

import { signal, computed, resource, Text, View, Scroll } from "nsh";
import { buwiz } from "buwiz";

type Props = { ticket?: string };
const PROPS = (buwiz.props || {}) as Props;
/** The taxpayer this card is for, or "" for all of them. */
const TICKET = typeof PROPS.ticket === "string" ? PROPS.ticket : "";

/** One return on the calendar (`ebirforms_calendar_get`'s events). */
interface Due {
  /** The return's ticket: what opens it in Forms. */
  key: string;
  taxpayer: string;
  form: string;
  form_name: string;
  period: string;
  label: string;
  due: string;
  status: "not_started" | "draft" | "queued" | "filed" | "confirmed" | "paid";
  status_detail: string;
  overdue: boolean;
  enabled: boolean;
}

const C = {
  pane: "bg-white dark:bg-[#161616]",
  card: "bg-[#f5f5f5] dark:bg-[#1f1f1f]",
  line: "border-black/10 dark:border-white/10",
  fg: "text-[#1c1c1c] dark:text-[#f5f5f5]",
  fg2: "text-[#1c1c1c99] dark:text-[#f5f5f599]",
  danger: "text-[#c62828] dark:text-[#ff6b6b]",
  warning: "text-[#b26a00] dark:text-[#ffb74d]",
  accent: "text-[#1680f3] dark:text-[#1884fc]",
  accentBg: "bg-[#1680f3] dark:bg-[#1884fc]",
};

const DAY = 86_400_000;
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const pad = (n: number) => String(n).padStart(2, "0");
const iso = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
const today = new Date();
/** `2026-11-03` -> `Nov 3`. */
function shortDate(day: string) {
  const [, m, d] = day.split("-").map(Number);
  return `${MONTHS[m - 1]} ${d}`;
}
/** Days from today to `day` (negative: past). */
function daysTo(day: string) {
  const [y, m, d] = day.split("-").map(Number);
  return Math.round((new Date(y, m - 1, d).getTime() - new Date(today.getFullYear(), today.getMonth(), today.getDate()).getTime()) / DAY);
}
function when(d: Due) {
  const n = daysTo(d.due);
  if (d.overdue) return `Overdue since ${shortDate(d.due)}`;
  if (n === 0) return "Due today";
  if (n === 1) return "Due tomorrow";
  return `Due ${shortDate(d.due)} · in ${n} days`;
}
const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

const ver = signal(0);
const who = signal(TICKET ? "" : "All taxpayers");
if (TICKET)
  buwiz.ticket
    .title(TICKET)
    .then((title) => who.set(title || "This taxpayer"))
    .catch(() => who.set("This taxpayer"));

const calendar = resource(
  () => ({ v: ver() }),
  async () => {
    const args: Record<string, string> = { from: iso(new Date(today.getTime() - 92 * DAY)), to: iso(new Date(today.getTime() + 183 * DAY)) };
    if (TICKET) args.tin = TICKET;
    try {
      const r: any = await buwiz.call("ebirforms_calendar_get", args);
      return (r?.result?.events || []) as Due[];
    } finally {
      // read (or failed): an inline card shows now (`inline.wait_ready`);
      // a Bot without buwiz.ready has none
      (buwiz as any).ready?.();
    }
  },
);

const DONE = ["filed", "confirmed", "paid"];
/** Returns still to file, overdue first, then by due date. */
const open = computed(() =>
  (calendar() || [])
    .filter((d) => d.enabled && !DONE.includes(d.status))
    .sort((a, b) => Number(b.overdue) - Number(a.overdue) || a.due.localeCompare(b.due)),
);
const inline = computed(() => buwiz.viewport().placement === "inline");
const shown = computed(() => open().slice(0, inline() ? 4 : 20));

function Btn(p: { label: string; onPress: () => void; primary?: boolean }) {
  return (
    <View class={`rounded-md items-center justify-center px-3 py-1 ${p.primary ? C.accentBg : ""}`} activeStyle={{ opacity: 0.7 }} onPress={() => p.onPress()}>
      <Text class={`text-sm font-semibold ${p.primary ? "text-white dark:text-white" : C.accent}`}>{p.label}</Text>
    </View>
  );
}

const failed = signal("");
function openReturn(d: Due) {
  failed.set("");
  buwiz.open({ app: "forms", ticket: d.key, props: { ticket: d.key, form: d.form, period: d.period, action: "file" } }).catch((e) => failed.set(errText(e)));
}
function openCalendar() {
  failed.set("");
  const target = TICKET ? { app: "calendar", ticket: TICKET, props: { ticket: TICKET } } : { app: "calendar", props: {} };
  buwiz.open(target).catch((e) => failed.set(errText(e)));
}

function Row(p: { d: Due }) {
  const d = p.d;
  return (
    <View class={`flex-row items-center gap-3 px-3 py-2 border-b ${C.line}`}>
      <View class="flex-1 gap-0.5">
        <Text class={`text-sm font-semibold ${C.fg}`}>{TICKET ? d.label : `${d.label} · ${d.taxpayer}`}</Text>
        <Text class={`text-xs ${d.overdue ? C.danger : daysTo(d.due) <= 7 ? C.warning : C.fg2}`}>{`${when(d)} · ${d.status_detail}`}</Text>
      </View>
      <Btn label="Open" onPress={() => openReturn(d)} />
    </View>
  );
}

export default function Deadlines() {
  return (
    <View style={{ flex: 1 }} class={`${C.pane} p-3 gap-2`}>
      <View class="flex-row items-center gap-2">
        <View class="flex-1">
          <Text class={`text-base font-semibold ${C.fg}`}>Next returns due</Text>
          <Text class={`text-xs ${C.fg2}`}>{who()}</Text>
        </View>
        <Btn label="Calendar" onPress={() => openCalendar()} />
      </View>
      <If when={Boolean(failed())}>
        <Text class={`text-xs ${C.danger}`}>{failed()}</Text>
      </If>
      <If when={Boolean(calendar.error())}>
        <View class="gap-1">
          <Text class={`text-sm ${C.danger}`}>{errText(calendar.error())}</Text>
          <Btn label="Retry" onPress={() => ver.set(ver() + 1)} />
        </View>
      </If>
      <If when={!calendar() && !calendar.error()}>
        <Text class={`text-sm ${C.fg2}`}>Loading…</Text>
      </If>
      <If when={Boolean(calendar()) && calendar()!.length === 0}>
        <Text class={`text-sm ${C.fg2}`}>No returns on the calendar yet: choose the forms this taxpayer files in their Tax Profile.</Text>
      </If>
      <If when={Boolean(calendar()?.length) && open().length === 0}>
        <Text class={`text-sm ${C.fg2}`}>Nothing due: every return on the calendar is filed.</Text>
      </If>
      <Scroll class="flex-1">
        <View class={`rounded-xl ${C.card}`}>
          <Each of={shown()} key={(d) => d.key}>
            {(d) => <Row d={d()} />}
          </Each>
        </View>
        <If when={open().length > shown().length}>
          <Text class={`text-xs px-1 pt-2 ${C.fg2}`}>{`${open().length - shown().length} more on the calendar`}</Text>
        </If>
      </Scroll>
    </View>
  );
}
