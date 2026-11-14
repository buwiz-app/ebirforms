// eBIRForms' Tax profiles card (spec 9.8), inline in the chat for
// `/taxprofile` and `/taxprofile list`: every saved taxpayer (archived ones
// left out) by name, TIN, type and RDO, each row opening its BIR Tax
// Profile; with none, New taxpayer.
//
// It reads the taxpayers itself when it mounts (its props' `tool`,
// `ebirforms_taxpayers_list`, an app's call): its props hold no taxpayer,
// so nothing about them reaches the chat, the assistant or the Bot's log of
// an open, and the TIN it shows is only on screen. A taxpayer whose TIN is
// not active on the Buwiz account gets BIR's chip for its state
// (`ebirforms_account_tins`, read after the list; none when active, unknown
// or signed out). It changes nothing.
//
// It does what the card kit (card-kit.tsx.in) does for the other cards
// (the read, `buwiz.ready`, `buwiz.fit`, the card frame and rows), written
// against nsh 0.3's types so `nsh check` passes; the kit's copy in
// card-dates and card-calc predates them.

import { signal, Text, View, Scroll } from "nsh";
import { buwiz } from "buwiz";

type CardProps = { card?: string; tool?: string; args?: Record<string, unknown> };
const P = (buwiz.props || {}) as CardProps;

const K = {
  pane: "bg-white dark:bg-[#161616]",
  soft: "bg-[#f5f5f5] dark:bg-[#1f1f1f]",
  fg: "text-[#1c1c1c] dark:text-[#f5f5f5]",
  fg2: "text-[#1c1c1c99] dark:text-[#f5f5f599]",
  fg3: "text-[#1c1c1c66] dark:text-[#f5f5f566]",
  accentBg: "bg-[#1680f3] dark:bg-[#1884fc]",
  danger: "text-[#c62828] dark:text-[#ff6b6b]",
  warn: "text-[#b26a00] dark:text-[#ffb74d]",
  accent: "text-[#1680f3] dark:text-[#1884fc]",
};
const tones: Record<string, [string, string]> = {
  danger: ["bg-[#c628281a] dark:bg-[#ff6b6b26]", K.danger],
  warning: ["bg-[#b26a001a] dark:bg-[#ffb74d26]", K.warn],
  info: ["bg-[#1680f31a] dark:bg-[#1884fc26]", K.accent],
};

/** A taxpayer's state chip on the Buwiz account, as BIR words it. */
interface StateChip {
  chip: string;
  tone: string;
}

interface TaxpayerRow {
  ticket: string;
  name: string;
  tin_display: string;
  taxpayer_type: string;
  rdo_code: string;
  rdo_office: string;
}

const errText = (e: unknown) => String((e as any)?.message || e || "Something went wrong").replace(/^Error:\s*/, "");
/** A tool's answer as data: its structured result. */
const answerOf = (r: any): any => (r && typeof r === "object" && "result" in r ? r.result : r);

/** The taxpayers, once read; null until then. */
const rows = signal<TaxpayerRow[] | null>(null);
const failure = signal("");
const busy = signal(false);
/** The state chips by taxpayer ticket: only taxpayers whose TIN is not active. */
const chips = signal<Record<string, StateChip>>({});

/** Tells the Bot the card's data is in: the manifest's `inline.wait_ready`
 *  holds its placeholder until then. */
const ready = () => (buwiz as any).ready?.();

async function load() {
  failure.set("");
  try {
    const got = answerOf(await buwiz.call!(P.tool || "ebirforms_taxpayers_list", (P.args || {}) as any));
    rows.set(Array.isArray(got?.taxpayers) ? got.taxpayers : []);
  } catch (e) {
    failure.set(errText(e));
    rows.set([]);
  } finally {
    ready();
  }
}
/** The chips, once the account's list is read; a failed read leaves none. */
async function loadChips() {
  try {
    const got = answerOf(await buwiz.call!("ebirforms_account_tins", {}));
    const found: Record<string, StateChip> = {};
    for (const r of Array.isArray(got?.rows) ? got.rows : []) {
      if (r?.ticket && r.chip) found[String(r.ticket)] = { chip: String(r.chip), tone: String(r.tone || "info") };
    }
    chips.set(found);
  } catch {
    chips.set({});
  }
}
load().then(loadChips);

/** Opens Tax Profile: on a taxpayer's ticket, or on its New taxpayer form. */
async function openProfile(ticket?: string) {
  if (busy()) return;
  busy.set(true);
  failure.set("");
  try {
    await buwiz.open!(ticket ? { app: "tax-profile", ticket, props: { ticket } } : { app: "tax-profile", props: { start: "new" } });
  } catch (e) {
    failure.set(errText(e));
  } finally {
    busy.set(false);
  }
}

/** The card's height as its content needs it (`buwiz.fit`): the column's,
 *  with the list's full height in place of the part the scroll shows. */
const fitParts = { column: 0, shown: 0, body: 0 };
let fitSent = 0;
let fitTimer: ReturnType<typeof setTimeout> | null = null;
function measured(part: keyof typeof fitParts, e: any) {
  fitParts[part] = (e.bounds ?? e).size.height;
  if (fitTimer !== null) return;
  fitTimer = setTimeout(() => {
    fitTimer = null;
    const { column, shown, body } = fitParts;
    const h = Math.ceil(column - shown + body);
    const fit = (buwiz as any).fit;
    if (!column || Math.abs(h - fitSent) < 1 || typeof fit !== "function") return;
    fitSent = h;
    fit(h);
  }, 0);
}

function Btn(p: { label: string; onPress: () => void }) {
  return (
    <View
      onPress={p.onPress}
      role="button"
      accessibilityLabel={p.label}
      activeStyle={{ opacity: 0.7 }}
      class={`rounded-md px-3 py-1 items-center justify-center border-2 border-transparent focus:border-[#1680f3] dark:focus:border-[#1884fc] ${K.accentBg}`}
    >
      <Text class="text-sm font-semibold text-white dark:text-white">{p.label}</Text>
    </View>
  );
}

/** A taxpayer's row: the name, then TIN, type and RDO; a press opens it. */
function Row(p: { t: TaxpayerRow }) {
  const rdo = () => (p.t.rdo_code ? `RDO ${p.t.rdo_code}${p.t.rdo_office ? ` ${p.t.rdo_office}` : ""}` : "");
  return (
    <View
      class={`flex-row items-center gap-2 px-2 py-1.5 rounded-lg border-2 border-transparent focus:border-[#1680f3] dark:focus:border-[#1884fc] ${K.soft}`}
      onPress={() => void openProfile(p.t.ticket)}
      role="button"
      accessibilityLabel={`Open ${p.t.name}'s tax profile`}
      activeStyle={{ opacity: 0.7 }}
    >
      <View class="flex-1">
        <Text class={`text-sm font-semibold ${K.fg}`} style={{ numberOfLines: 1 }}>{p.t.name}</Text>
        <Text class={`text-xs ${K.fg2}`} style={{ numberOfLines: 1 }}>
          {[p.t.tin_display, p.t.taxpayer_type, rdo()].filter(Boolean).join(" · ")}
        </Text>
      </View>
      <If when={Boolean(chips()[p.t.ticket])}>
        <View class={`rounded-full px-2 py-0.5 ${(tones[chips()[p.t.ticket]?.tone] || tones.info)[0]}`}>
          <Text class={`text-xs font-semibold ${(tones[chips()[p.t.ticket]?.tone] || tones.info)[1]}`}>{chips()[p.t.ticket]?.chip || ""}</Text>
        </View>
      </If>
      <Text class={`text-sm ${K.fg3}`}>›</Text>
    </View>
  );
}

export default function PeopleCard() {
  const list = () => rows() || [];
  const sub = () => (rows() === null ? "Loading…" : list().length === 1 ? "1 taxpayer" : `${list().length} taxpayers`);
  return (
    <View style={{ flex: 1 }} class={K.pane}>
      <View style={{ flexShrink: 1, minHeight: 0 }} class="p-3 gap-2" onLayout={(e: any) => measured("column", e)}>
        <View class="flex-row items-start gap-2">
          <View class="flex-1">
            <Text class={`text-base font-semibold ${K.fg}`} style={{ numberOfLines: 1 }}>Tax profiles</Text>
            <Text class={`text-xs ${K.fg2}`} style={{ numberOfLines: 1 }}>{sub()}</Text>
          </View>
          <If when={busy()}>
            <Text class={`text-xs ${K.fg3}`}>Opening…</Text>
          </If>
        </View>
        <If when={Boolean(failure())}>
          <Text class={`text-xs ${K.danger}`}>{failure()}</Text>
        </If>
        <If when={rows() !== null && list().length === 0 && !failure()}>
          <View class="gap-2 py-2">
            <Text class={`text-sm ${K.fg}`}>No tax profiles yet.</Text>
            <View class="flex-row flex-wrap items-center gap-2">
              <Btn label="New taxpayer" onPress={() => void openProfile()} />
              <Text class={`text-xs ${K.fg2}`}>or type /taxprofile create</Text>
            </View>
          </View>
        </If>
        <Scroll style={{ flexGrow: 0, flexShrink: 1, minHeight: 0 }} onLayout={(e: any) => measured("shown", e)}>
          <View class="gap-1.5" onLayout={(e: any) => measured("body", e)}>
            <Each of={list()} key={(t) => t.ticket}>
              {(t) => <Row t={t()} />}
            </Each>
          </View>
        </Scroll>
      </View>
    </View>
  );
}
