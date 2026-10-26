// BIR Tax Profile — one taxpayer's profile as a Rox mini app.
//
// Port of bir-shell's taxpayer page (app/shell/taxpayer.js: header, Profile /
// Forms / Templates tabs, New branch) plus that taxpayer's dues and returns
// from Home and Filings (app/shell/home.js, filings.js). It is focused on the
// one taxpayer Rox opened it for (props.tin / props.scope); there is no
// taxpayer switcher. Opening a return hands off to the BIR Forms mini app.
//
// Desktop-only pieces left out: profile export (host save dialog), the
// right-panel quick edit (the section editors cover it), Recent, and "Use as
// template" on a filed return (create_template_from_return is refused under a
// taxpayer scope).

import { signal, computed, resource, effect, cleanup, Text, View, Scroll, Input } from "nsh";
import type { Signal, Accessor, JSX } from "nsh";
import { Clipboard } from "nsh/clipboard";

// ============================================================ host calls
// Every bir.* call this app makes goes through `bir` and is listed in `api`.

type Props = { server?: string; tin?: string; scope?: string };
const PROPS: Props = ((globalThis as any).roxProps || {}) as Props;
/** The taxpayer scope sent with every call; follows a TIN move. */
let SCOPE: string | undefined = PROPS.scope || (PROPS.tin ? String(PROPS.tin).slice(0, 9) : undefined);

async function bir<T = any>(fn: string, ...args: unknown[]): Promise<T> {
  const p = (globalThis as any).roxProps || {};
  const r = await fetch("http://127.0.0.1:8787/mcp/call", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ server: p.server || "bir", tool: "bir_call", args: { fn, args, scope: SCOPE } }),
  }).then((x) => x.json());
  if (!r.ok) throw new Error(r.error || "BIR call failed");
  return r.result as T;
}

type TaxpayerType = "Individual" | "Corporation" | "Partnership" | "Cooperative" | "Estate" | "Trust";
type BranchFieldName = "rdo_code" | "registered_address" | "zip_code" | "phone" | "email";
type BranchFacts = Record<BranchFieldName, string>;
interface Branch {
  tin: string;
  tin_display: string;
  tin_root: string;
  code: string;
  name: string;
  is_head_office: boolean;
  facts: BranchFacts;
  inherited: BranchFieldName[];
}
interface Taxpayer {
  tin_root: string;
  tin_display: string;
  name: string;
  taxpayer_type: TaxpayerType;
  birth_date: string | null;
  needs_grouping_review: boolean;
  archived: boolean;
  branches: Branch[];
}
type FieldUpdate = Partial<Record<BranchFieldName, string | null>>;
interface TaxYearBounds { min: number; max: number }
interface AddedTaxYear { year: number; copied_from: number | null; forms: string[] }
interface YearlyFormsChange { added: string[]; removed: string[]; forms: string[] }
interface StarterPackInfo { id: string; label: string; forms: string[] }
interface FormTemplateSummary { form_code: string; updated_at: string; answers_kept: number; marked: number; source: string }
interface RdoOption { code: string; description: string }
interface ScreenFormIndexEntry { c: string; t: string; rev: string; freq: string; pages: number; n: number; q: number }
interface InboxState {
  tin: string;
  inbox_email: string;
  auth_method: "AppPassword" | "GoogleOAuth";
  tracking_enabled: boolean;
  imap_login: string;
  imap_host: string;
  has_app_password: boolean;
  oauth_connected: boolean;
  oauth_inbox_email: string | null;
  oauth_updated_at: string | null;
  still_pending: boolean;
  matching_tins: string[];
  last_check_at: string | null;
  last_check_status: string | null;
  last_check_log: string | null;
}
interface InboxVerifyResult { ok: boolean; message: string; state: InboxState }
interface InboxCheckResult { ok: boolean; still_pending: boolean; error: string | null; state: InboxState }
interface SendGrant { tin: string; form: string; granted_at: string }
interface AuthState {
  mode: "solo" | "helpdesk" | null;
  admin_root: string | null;
  has_admin_pin: boolean;
  has_admin_totp: boolean;
  has_recovery_key: boolean;
  mac_login_available: boolean;
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
  frequency: string;
  taxable_year: number;
  period: number;
  period_label: string;
  due_date: string;
  status: string;
  draft_id: number | null;
  return_key: number | null;
}
interface AsNeededReturn { draft_id: number; period: number; return_key: number | null; status: string; updated_at: string }
interface AsNeededForm {
  tin: string;
  tin_root: string;
  branch_code: string;
  branch_name: string;
  taxpayer_name: string;
  branch_count: number;
  form_code: string;
  form_name: string;
  taxable_year: number;
  trigger: string | null;
  due_rule: string;
  undated: AsNeededReturn[];
}
interface ReturnRow {
  id: number;
  tin: string;
  tin_root: string;
  branch_code: string;
  branch_name: string;
  taxpayer_name: string;
  branch_count: number;
  form_code: string;
  taxable_year: number;
  period: number;
  period_label: string;
  status: string;
  updated_at: string;
  return_key: number | null;
}

const api = {
  authState: () => bir<AuthState>("auth_state"),
  getTaxpayer: (root: string) => bir<Taxpayer | null>("get_taxpayer", root),
  updateTaxpayer: (root: string, update: { name?: string; taxpayer_type?: TaxpayerType }) => bir<Taxpayer>("update_taxpayer", root, update),
  moveTaxpayerTin: (oldRoot: string, newRoot: string) => bir<Taxpayer>("move_taxpayer_tin", oldRoot, newRoot),
  createBranch: (root: string, input: { branch_code: string; name: string; fields?: FieldUpdate }) => bir<Branch>("create_branch", root, input),
  updateBranch: (tin: string, update: { name?: string; fields?: FieldUpdate }) => bir<Branch>("update_branch", tin, update),
  moveBranchCode: (tin: string, code: string) => bir<Branch>("move_branch_code", tin, code),
  suggestNextBranchCode: (root: string) => bir<string>("suggest_next_branch_code", root),
  setTaxpayerArchived: (root: string, archived: boolean) => bir<null>("set_taxpayer_archived", root, archived),
  deleteTaxpayer: (root: string) => bir<null>("delete_taxpayer", root),
  listRdos: () => bir<RdoOption[]>("list_rdos"),
  listZipCodes: () => bir<string[]>("list_zip_codes"),
  taxYearBounds: () => bir<TaxYearBounds>("tax_year_bounds"),
  listTaxYears: (tin: string) => bir<number[]>("list_tax_years", tin),
  addTaxYear: (tin: string, year: number, copy: boolean) => bir<AddedTaxYear>("add_tax_year", tin, year, copy),
  yearlyForms: (tin: string, year: number) => bir<string[]>("yearly_forms", tin, year),
  setYearlyForms: (tin: string, year: number, codes: string[]) => bir<YearlyFormsChange>("set_yearly_forms", tin, year, codes),
  starterPacks: (tin: string) => bir<StarterPackInfo[]>("starter_packs", tin),
  screenFormsIndex: () => bir<ScreenFormIndexEntry[]>("screen_forms_index"),
  listFormTemplates: (tin: string) => bir<FormTemplateSummary[]>("list_form_templates", tin),
  createTemplateFromLatest: (tin: string, code: string) => bir<FormTemplateSummary>("create_template_from_latest", tin, code),
  deleteFormTemplate: (tin: string, code: string) => bir<boolean>("delete_form_template", tin, code),
  inboxState: (tin: string) => bir<InboxState>("inbox_state", tin),
  setInboxAppPassword: (tin: string, input: { imap_email: string; imap_host?: string | null; app_password?: string | null }) =>
    bir<InboxState>("set_inbox_app_password", tin, input),
  setInboxTracking: (tin: string, enabled: boolean) => bir<InboxState>("set_inbox_tracking", tin, enabled),
  disconnectInboxOauth: (tin: string) => bir<InboxState>("disconnect_inbox_oauth", tin),
  connectInboxOauth: (tin: string) => bir<InboxState>("connect_inbox_oauth", tin),
  testInboxConnection: (tin: string) => bir<InboxVerifyResult>("test_inbox_connection", tin),
  checkInboxNow: (tin: string) => bir<InboxCheckResult>("check_inbox_now", tin),
  listSendGrants: (tin: string) => bir<SendGrant[]>("list_send_grants", tin),
  grantSend: (tin: string, form: string) => bir<SendGrant>("grant_send", tin, form),
  revokeSend: (tin: string, form: string) => bir<null>("revoke_send", tin, form),
  deadlinesBetween: (from: string, to: string) => bir<DeadlineRow[]>("deadlines_between", from, to),
  asNeededForms: (year: number) => bir<AsNeededForm[]>("as_needed_forms", year),
  listReturns: (which: "filed" | "saved" | "all") => bir<ReturnRow[]>("list_returns", which),
  cancelQueued: (tin: string, code: string, year: number, quarter: number | null) => bir<null>("cancel_queued", tin, code, year, quarter),
};

/** Which return the forms app opens on: a form, and with year/period/key one return. */
interface FormsTarget {
  form_code?: string;
  year?: number;
  period?: number;
  key?: number | null;
}

/** The BIR Forms mini app for one branch, optionally on one return. */
function formsLink(tin: string, scope: string, target: FormsTarget = {}): string {
  return `rox://app/${PROPS.server || "bir"}/forms?props=${encodeURIComponent(JSON.stringify({ tin, scope, ...target }))}`;
}

/** Opens a `rox://app/…` link through Rox (nsh has no open-URL API). */
async function openRoxLink(url: string): Promise<boolean> {
  try {
    const r = await fetch("http://127.0.0.1:8787/mcp/open", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url }),
    }).then((x) => x.json());
    return !!r.ok;
  } catch {
    return false;
  }
}

// ============================================================ helpers

const TAXPAYER_TYPES: TaxpayerType[] = ["Individual", "Corporation", "Partnership", "Cooperative", "Estate", "Trust"];
const FREQS = ["All", "Monthly", "Quarterly", "Annual", "As needed"];
const ADDRESS_FIELDS: [BranchFieldName, string, boolean][] = [
  ["registered_address", "Registered address", true],
  ["zip_code", "ZIP code", false],
  ["phone", "Phone", false],
  ["email", "Email", true],
];
const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function errorText(e: unknown): string {
  return String(e && (e as any).message ? (e as any).message : e)
    .replace(/^Error:\s*/, "")
    .replace(/^`[^`]+`:\s*/, "");
}
function formatTin(root: string, branch?: string): string {
  const r = String(root || "").replace(/\D/g, "");
  const base = r.length >= 9 ? `${r.slice(0, 3)}-${r.slice(3, 6)}-${r.slice(6, 9)}` : r;
  return branch ? `${base}-${branch}` : base;
}
function isoDate(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}
function addDays(d: Date, n: number): Date {
  const x = new Date(d.getTime());
  x.setDate(x.getDate() + n);
  return x;
}
function daysUntil(iso: string): number {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const [y, m, d] = iso.split("-").map(Number);
  return Math.round((new Date(y, m - 1, d).getTime() - today.getTime()) / 86400000);
}
function shortDate(text: string | null | undefined): string {
  if (!text) return "";
  const ts = /^(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2})/.exec(String(text));
  if (ts) {
    const local = new Date(Date.UTC(+ts[1], +ts[2] - 1, +ts[3], +ts[4], +ts[5]));
    return `${MONTHS[local.getMonth()]} ${local.getDate()}, ${local.getFullYear()}`;
  }
  const [y, m, d] = String(text).split(/[ T]/)[0].split("-").map(Number);
  if (!y || !m || !d) return String(text);
  return `${MONTHS[m - 1]} ${d}, ${y}`;
}
function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}
function searchText(f: ScreenFormIndexEntry): string {
  const vat = /Value-Added/i.test(f.t) ? " vat" : "";
  return `${f.c} ${f.c.replace(/-/g, "")} ${f.t}${vat}`.toLowerCase();
}
function statusKind(status: string): "draft" | "queued" | "filed" | "not_started" {
  if (status === "Draft") return "draft";
  if (status === "Queued") return "queued";
  if (status === "Submitted" || status === "Confirmed" || status === "Paid") return "filed";
  return "not_started";
}
function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  return ((parts[0] || "?")[0] + (parts.length > 1 ? parts[parts.length - 1][0] : "")).toUpperCase();
}
type Option = { value: string; label: string };
/** What is wrong with an RDO, ZIP code or email as typed, or null. Empty is fine. */
function factError(field: BranchFieldName, value: string, rdos: Option[]): string | null {
  const v = value.trim();
  if (!v) return null;
  if (field === "rdo_code" && rdos.length && !rdos.some((o) => o.value === v)) return "Pick an RDO from the list. Type its code or district to search.";
  if (field === "zip_code" && !/^\d{4}$/.test(v)) return "ZIP code is 4 digits, like 1226. Type a code or a place to search.";
  if (field === "email" && !EMAIL_RE.test(v)) return "Type an email like name@example.com.";
  return null;
}

// ============================================================ UI kit (Sand theme)

const C = {
  pane: "bg-white dark:bg-[#161616]",
  card: "bg-[#f5f5f5] dark:bg-[#1f1f1f]",
  inset: "bg-black/5 dark:bg-white/5",
  fg: "text-[#1c1c1c] dark:text-[#f5f5f5]",
  fg2: "text-[#1c1c1c99] dark:text-[#f5f5f599]",
  fg3: "text-[#1c1c1c66] dark:text-[#f5f5f566]",
  line: "border border-black/10 dark:border-white/10",
  accent: "text-[#1680f3] dark:text-[#1884fc]",
  accentBg: "bg-[#1680f3] dark:bg-[#1884fc]",
  danger: "text-[#d92d20] dark:text-[#ff6961]",
  success: "text-[#16833a] dark:text-[#3fb950]",
  warning: "text-[#b25e00] dark:text-[#f0a742]",
};

type Variant = "primary" | "default" | "ghost" | "danger";
function Button(p: { label: string; onPress: () => void; variant?: Variant; disabled?: boolean }) {
  const box = () =>
    p.variant === "primary"
      ? C.accentBg
      : p.variant === "ghost"
        ? ""
        : p.variant === "danger"
          ? "bg-[#d92d20] dark:bg-[#e5484d]"
          : `${C.card} ${C.line}`;
  const ink = () => (p.variant === "primary" || p.variant === "danger" ? "text-white dark:text-white" : p.variant === "ghost" ? C.accent : C.fg);
  return (
    <View
      onPress={() => !p.disabled && p.onPress()}
      activeStyle={{ opacity: 0.7 }}
      class={`px-3 py-1 rounded-md items-center justify-center ${box()} ${p.disabled ? "opacity-50" : ""}`}
      style={{ minHeight: 28 }}
    >
      <Text class={`text-xs font-semibold ${ink()}`}>{p.label}</Text>
    </View>
  );
}

function Chip(p: { label: string; tone?: "accent" | "success" | "warning" | "danger" | "plain" }) {
  const tone = () =>
    p.tone === "accent"
      ? `bg-[#1680f31a] dark:bg-[#1884fc26] ${C.accent}`
      : p.tone === "success"
        ? `bg-[#16833a1a] dark:bg-[#3fb95026] ${C.success}`
        : p.tone === "warning"
          ? `bg-[#b25e001a] dark:bg-[#f0a74226] ${C.warning}`
          : p.tone === "danger"
            ? `bg-[#d92d201a] dark:bg-[#ff696126] ${C.danger}`
            : `${C.inset} ${C.fg2}`;
  return (
    <View class={`px-2 py-0.5 rounded-md self-start ${tone().split(" ").filter((c) => c.startsWith("bg-") || c.startsWith("dark:bg-")).join(" ")}`}>
      <Text class={`text-xs font-medium ${tone().split(" ").filter((c) => !(c.startsWith("bg-") || c.startsWith("dark:bg-"))).join(" ")}`}>{p.label}</Text>
    </View>
  );
}

function StatusChip(p: { status: string }) {
  const k = () => statusKind(p.status);
  return (
    <Chip
      label={k() === "draft" ? "Draft" : k() === "queued" ? "Queued" : k() === "filed" ? "Filed" : "Not started"}
      tone={k() === "filed" ? "success" : k() === "not_started" ? "plain" : "accent"}
    />
  );
}

function Segmented<T extends string | number>(p: { options: { value: T; label: string }[]; value: T | null; onChange: (v: T) => void }) {
  return (
    <View class={`flex-row flex-wrap gap-0.5 p-0.5 rounded-md self-start ${C.inset}`}>
      <Each of={p.options}>
        {(o) => (
          <View
            onPress={() => p.onChange(o().value)}
            activeStyle={{ opacity: 0.7 }}
            class={`px-3 py-1 rounded-md ${o().value === p.value ? `${C.pane} border border-black/10 dark:border-white/10` : ""}`}
          >
            <Text class={`text-xs font-medium ${o().value === p.value ? C.fg : C.fg2}`}>{o().label}</Text>
          </View>
        )}
      </Each>
    </View>
  );
}

function Switch(p: { on: boolean; onChange: (v: boolean) => void; label: string }) {
  return (
    <View onPress={() => p.onChange(!p.on)} activeStyle={{ opacity: 0.7 }} class="flex-row items-center gap-2" accessibilityLabel={p.label}>
      <View class={`rounded-full p-0.5 ${p.on ? C.accentBg : "bg-black/20 dark:bg-white/20"}`} style={{ width: 34, height: 20, alignItems: p.on ? "flex-end" : "flex-start" }}>
        <View class="rounded-full bg-white dark:bg-white" style={{ width: 16, height: 16 }} />
      </View>
      <Text class={`text-xs ${C.fg2}`}>{p.label}</Text>
    </View>
  );
}

function Checkbox(p: { on: boolean; onChange: (v: boolean) => void; label?: string }) {
  return (
    <View onPress={() => p.onChange(!p.on)} activeStyle={{ opacity: 0.7 }} class="flex-row items-center gap-2">
      <View
        class={`rounded items-center justify-center ${p.on ? C.accentBg : `${C.pane} border border-black/20 dark:border-white/20`}`}
        style={{ width: 18, height: 18 }}
      >
        <If when={p.on}>
          <Text class="text-xs font-bold text-white dark:text-white">✓</Text>
        </If>
      </View>
      <If when={p.label}>
        <Text class={`text-sm ${C.fg}`}>{p.label}</Text>
      </If>
    </View>
  );
}

function Label(p: { text: string }) {
  return <Text class={`text-xs ${C.fg2}`}>{p.text}</Text>;
}
function ErrorText(p: { text: string | null | undefined }) {
  return (
    <If when={p.text}>
      <Text class={`text-xs ${C.danger}`}>{p.text}</Text>
    </If>
  );
}

/** A read-only field, or the edit control passed as children while editing. */
function Field(p: { label: string; value?: string | null; full?: boolean; inherited?: boolean; children?: JSX.Element }) {
  return (
    <View class="gap-1" style={{ flexGrow: 1, flexBasis: p.full ? "100%" : 220, minWidth: 200 }}>
      <View class="flex-row items-center gap-2">
        <Label text={p.label} />
        <If when={p.inherited}>
          <Text class={`text-xs ${C.fg3}`}>· From head office</Text>
        </If>
      </View>
      <If when={p.children} else={<Text class={`text-sm ${p.value ? C.fg : C.fg3}`}>{p.value || "Not set"}</Text>}>
        {p.children}
      </If>
    </View>
  );
}

function TextInput(p: {
  value: Signal<string>;
  placeholder?: string;
  error?: boolean;
  disabled?: boolean;
  secure?: boolean;
  digits?: number;
}) {
  return (
    <Input
      value={p.value()}
      onChange={(v) => p.value.set(p.digits ? v.replace(/\D/g, "").slice(0, p.digits) : v)}
      placeholder={p.placeholder}
      readonly={p.disabled}
      masked={p.secure}
      keyboardType={p.digits ? "number-pad" : "default"}
      class={`placeholder:text-[#8e8e93] rounded-md px-3 py-2 text-sm ${C.fg} ${p.disabled ? C.inset : C.pane} border ${p.error ? "border-[#d92d20] dark:border-[#ff6961]" : "border-black/10 dark:border-white/10"} ${p.disabled ? "opacity-60" : ""}`}
    />
  );
}

/** A pick list over a reference list (RDO, ZIP): a search box and the first matches. */
function Combo(p: { value: Signal<string>; options: Option[]; placeholder: string; error?: boolean; disabled?: boolean }) {
  const open = signal(false);
  const query = signal("");
  const current = computed(() => p.options.find((o) => o.value === p.value()));
  const matches = computed(() => {
    const q = query().trim().toLowerCase();
    return p.options.filter((o) => !q || o.label.toLowerCase().includes(q)).slice(0, 8);
  });
  return (
    <View class="gap-1">
      <View
        onPress={() => !p.disabled && open.set(!open())}
        activeStyle={{ opacity: 0.7 }}
        class={`flex-row items-center justify-between rounded-md px-3 py-2 border ${p.disabled ? `${C.inset} opacity-60` : C.pane} ${p.error ? "border-[#d92d20] dark:border-[#ff6961]" : "border-black/10 dark:border-white/10"}`}
      >
        <Text class={`text-sm flex-1 ${current() || p.value() ? C.fg : C.fg3}`} numberOfLines={1}>
          {current()?.label || p.value() || p.placeholder}
        </Text>
        <Text class={`text-xs ${C.fg3}`}>{open() ? "▴" : "▾"}</Text>
      </View>
      <If when={open() && !p.disabled}>
        <View class={`rounded-md p-2 gap-1 ${C.card} ${C.line}`}>
          <Input
            value={query()}
            onChange={(v) => query.set(v)}
            placeholder={p.placeholder}
            autoFocus
            class={`placeholder:text-[#8e8e93] rounded-md px-3 py-2 text-sm ${C.fg} ${C.pane} border border-black/10 dark:border-white/10`}
          />
          <Each of={matches()}>
            {(o) => (
              <View
                onPress={() => {
                  p.value.set(o().value);
                  query.set("");
                  open.set(false);
                }}
                activeStyle={{ opacity: 0.6 }}
                class={`px-2 py-1 rounded-md ${o().value === p.value() ? C.inset : ""}`}
              >
                <Text class={`text-sm ${C.fg}`} numberOfLines={1}>{o().label}</Text>
              </View>
            )}
          </Each>
          <If when={!matches().length}>
            <Text class={`text-xs px-2 py-1 ${C.fg3}`}>{p.options.length ? "No match." : "The list did not load."}</Text>
          </If>
          <View class="flex-row justify-end">
            <Button
              label="Clear"
              variant="ghost"
              onPress={() => {
                p.value.set("");
                open.set(false);
              }}
            />
          </View>
        </View>
      </If>
    </View>
  );
}

/** A collapsible profile section with an Edit action (taxpayer.js `Disclosure`). */
function Section(p: {
  title: string;
  meta: string;
  open: boolean;
  onToggle: () => void;
  onEdit?: (() => void) | null;
  editing?: boolean;
  children?: JSX.Element;
}) {
  return (
    <View class={`rounded-xl ${C.line} ${C.pane} overflow-hidden`}>
      <View onPress={p.onToggle} activeStyle={{ opacity: 0.8 }} class="flex-row items-center gap-3 px-4 py-3">
        <Text class={`text-xs ${C.fg3}`}>{p.open ? "▾" : "▸"}</Text>
        <View class="flex-1 gap-0.5">
          <Text class={`text-sm font-semibold ${C.fg}`}>{p.title}</Text>
          <Text class={`text-xs ${C.fg2}`} numberOfLines={1}>{p.meta}</Text>
        </View>
        <If when={p.onEdit && !p.editing}>
          <Button
            label="Edit"
            variant="ghost"
            onPress={() => {
              if (!p.open) p.onToggle();
              p.onEdit?.();
            }}
          />
        </If>
      </View>
      <If when={p.open}>
        <View class="px-4 pb-4 gap-3" style={{ paddingLeft: 36 }}>
          {p.children}
        </View>
      </If>
    </View>
  );
}

function FormActions(p: { onCancel: () => void; onSave: () => void; saveLabel?: string; busy?: boolean }) {
  return (
    <View class="flex-row justify-end gap-2 mt-2">
      <Button label="Cancel" variant="ghost" onPress={p.onCancel} />
      <Button label={p.busy ? "Saving…" : p.saveLabel || "Save"} variant="primary" disabled={p.busy} onPress={p.onSave} />
    </View>
  );
}

function ListGroup(p: { children?: JSX.Element }) {
  return <View class={`rounded-xl ${C.line} ${C.pane} overflow-hidden`}>{p.children}</View>;
}
function Row(p: { children?: JSX.Element; first?: boolean }) {
  return (
    <View
      class={`flex-row items-center gap-3 px-4 py-2 ${p.first ? "" : "border-t border-black/5 dark:border-white/5"}`}
      style={{ minHeight: 48 }}
    >
      {p.children}
    </View>
  );
}
function NameCell(p: { title: string; sub?: string; dim?: boolean }) {
  return (
    <View class="flex-1 gap-0.5" style={{ minWidth: 0 }}>
      <Text class={`text-sm ${p.dim ? C.fg3 : C.fg}`} numberOfLines={1}>{p.title}</Text>
      <If when={p.sub}>
        <Text class={`text-xs ${C.fg2}`} numberOfLines={1}>{p.sub}</Text>
      </If>
    </View>
  );
}
function Code(p: { text: string; dim?: boolean }) {
  return (
    <Text class={`text-sm font-semibold ${p.dim ? C.fg3 : C.fg}`} style={{ width: 68, fontFamily: "Menlo" }}>
      {p.text}
    </Text>
  );
}
function EmptyRow(p: { text: string; children?: JSX.Element }) {
  return (
    <View class="flex-row flex-wrap items-center justify-between gap-3 px-4 py-4">
      <Text class={`text-sm flex-1 ${C.fg2}`}>{p.text}</Text>
      {p.children}
    </View>
  );
}
function SectionTitle(p: { text: string }) {
  return <Text class={`text-xs font-semibold mt-4 mb-1 ${C.fg2}`}>{p.text.toUpperCase()}</Text>;
}
/** A failed read: what went wrong and a retry. */
function LoadError(p: { error: unknown; retry: () => void; what: string }) {
  return (
    <If when={p.error}>
      <View class={`flex-row flex-wrap items-center gap-3 rounded-lg px-3 py-2 ${C.line} bg-[#d92d200d] dark:bg-[#ff69611a]`}>
        <Text class={`text-xs flex-1 ${C.danger}`}>{`Could not load ${p.what}: ${errorText(p.error)}`}</Text>
        <Button label="Retry" onPress={p.retry} />
      </View>
    </If>
  );
}

// ============================================================ the app

export default function R() {
  const startTin = String(PROPS.tin || "").replace(/\D/g, "");
  if (startTin.length < 9) {
    return (
      <View class={`flex-1 items-center justify-center p-6 ${C.pane}`} style={{ minHeight: 400 }}>
        <View class={`rounded-2xl p-6 gap-2 items-center ${C.card} ${C.line}`} style={{ maxWidth: 420 }}>
          <Text class={`text-base font-semibold ${C.fg}`}>Open this from a taxpayer</Text>
          <Text class={`text-sm text-center ${C.fg2}`}>
            The tax profile shows one taxpayer. Ask Rox for a taxpayer's profile, or open it from a taxpayer in BIR.
          </Text>
        </View>
      </View>
    );
  }

  // ---------------------------------------------------------------- selection and data
  const root = signal(startTin.slice(0, 9));
  const code = signal(startTin.length >= 14 ? startTin.slice(9, 14) : "00000");
  const ver = signal(0);
  const bump = () => ver.update((v) => v + 1);
  const tab = signal<"profile" | "forms" | "templates" | "returns">("profile");
  const deleted = signal<string | null>(null);

  const toast = signal<{ text: string; ok: boolean } | null>(null);
  let toastTimer: number | undefined;
  const say = (text: string, ok = true) => {
    toast.set({ text, ok });
    clearTimeout(toastTimer);
    if (ok) toastTimer = setTimeout(() => toast.set(null), 5000);
  };
  cleanup(() => clearTimeout(toastTimer));

  /** The forms hand-off: Rox opens the link; when it can't, the link is copied and shown. */
  const handoff = signal<{ title: string; url: string } | null>(null);
  const openForms = async (title: string, tin: string, rootOf: string, target: FormsTarget = {}) => {
    const url = formsLink(tin, rootOf, target);
    if (await openRoxLink(url)) return;
    try {
      void Clipboard.writeText(url);
    } catch {}
    handoff.set({ title, url });
  };

  const tp = resource(
    () => !deleted() && { root: root(), v: ver() },
    (s) => api.getTaxpayer(s.root),
  );
  const auth = resource(() => api.authState());
  const bounds = resource(() => api.taxYearBounds(), { initialValue: { min: 2010, max: new Date().getFullYear() + 1 } });
  const index = resource(() => api.screenFormsIndex(), { initialValue: [] as ScreenFormIndexEntry[] });
  const rdos = resource(async () => (await api.listRdos()).map((r) => ({ value: r.code, label: `${r.code} - ${r.description}` })), {
    initialValue: [] as Option[],
  });
  const zips = resource(async () => (await api.listZipCodes()).map((z) => ({ value: z.split(" - ")[0].trim(), label: z })), {
    initialValue: [] as Option[],
  });

  const branch = computed<Branch | null>(() => {
    const t = tp();
    if (!t) return null;
    return t.branches.find((b) => b.code === code()) || t.branches.find((b) => b.is_head_office) || t.branches[0] || null;
  });
  const head = computed(() => tp()?.branches.find((b) => b.is_head_office) || null);
  const tin = computed(() => branch()?.tin || "");

  const years = resource(
    () => !!tin() && { tin: tin(), v: ver() },
    (s) => api.listTaxYears(s.tin),
    { initialValue: [] as number[] },
  );
  const selYear = signal<number | null>(null);
  /** The selected year, kept valid: this year if the branch has it, else its latest. */
  const year = computed<number | null>(() => {
    const ys = years();
    const sel = selYear();
    if (sel !== null && ys.includes(sel)) return sel;
    const now = new Date().getFullYear();
    return ys.includes(now) ? now : ys.length ? ys[ys.length - 1] : null;
  });
  const yearForms = resource(
    () => !!tin() && year() !== null && { tin: tin(), year: year() as number, v: ver() },
    (s) => api.yearlyForms(s.tin, s.year),
    { initialValue: [] as string[] },
  );
  const templates = resource(
    () => !!tin() && { tin: tin(), v: ver() },
    (s) => api.listFormTemplates(s.tin),
    { initialValue: [] as FormTemplateSummary[] },
  );
  const packs = resource(
    () => !!tin() && tin(),
    (t) => api.starterPacks(t),
    { initialValue: [] as StarterPackInfo[] },
  );
  const yearDeadlines = resource(
    () => year() !== null && { year: year() as number, v: ver() },
    (s) => api.deadlinesBetween(`${s.year}-01-01`, `${s.year + 1}-12-31`),
    { initialValue: [] as DeadlineRow[] },
  );
  const inbox = resource(
    () => !!tin() && { tin: tin(), v: ver() },
    (s) => api.inboxState(s.tin),
  );
  const grants = resource(
    () => !!tin() && { tin: tin(), v: ver() },
    (s) => api.listSendGrants(s.tin),
    { initialValue: [] as SendGrant[] },
  );
  const dues = resource(
    () => ({ root: root(), v: ver() }),
    async (s) => {
      const today = new Date();
      const rows = await api.deadlinesBetween(isoDate(addDays(today, -30)), isoDate(addDays(today, 30)));
      return rows.filter((d) => d.tin_root === s.root && (daysUntil(d.due_date) >= 0 || d.status === "Not started" || d.status === "Draft"));
    },
    { initialValue: [] as DeadlineRow[] },
  );
  const asNeeded = resource(
    () => ({ root: root(), v: ver() }),
    async (s) => (await api.asNeededForms(new Date().getFullYear())).filter((f) => f.tin_root === s.root),
    { initialValue: [] as AsNeededForm[] },
  );
  const returns = resource(
    () => ({ root: root(), v: ver() }),
    async (s) => (await api.listReturns("all")).filter((r) => r.tin_root === s.root),
    { initialValue: [] as ReturnRow[] },
  );

  /** A write: busy while it runs, the data re-read after it, the error said. */
  const busy = signal(false);
  const write = async (fn: () => Promise<unknown>, done?: string): Promise<boolean> => {
    if (busy()) return false;
    busy.set(true);
    try {
      await fn();
      if (done) say(done);
      return true;
    } catch (e) {
      say(errorText(e), false);
      return false;
    } finally {
      busy.set(false);
      bump();
    }
  };

  // ---------------------------------------------------------------- profile sections (taxpayer.js startEdit / saveSection)
  const open = signal<Record<string, boolean>>({ taxpayer: true, branch: true, address: true, branches: true, inbox: false, agent: false });
  const toggle = (id: string) => {
    open.update((o) => ({ ...o, [id]: !o[id] }));
    if (editSec() === id && !open()[id]) editSec.set(null);
  };
  const editSec = signal<"taxpayer" | "branch" | "address" | null>(null);
  const inName = signal("");
  const inTin = signal("");
  const inType = signal<TaxpayerType>("Individual");
  const inCode = signal("");
  const inBranchName = signal("");
  const facts: Record<BranchFieldName, Signal<string>> = {
    rdo_code: signal(""),
    registered_address: signal(""),
    zip_code: signal(""),
    phone: signal(""),
    email: signal(""),
  };
  const inherit = signal<Partial<Record<BranchFieldName, boolean>>>({});
  const factErrors = signal<Partial<Record<BranchFieldName, string>>>({});
  const tinError = signal<string | null>(null);

  const startEdit = (sec: "taxpayer" | "branch" | "address") => {
    const t = tp();
    const b = branch();
    if (!t || !b) return;
    editSec.set(sec);
    tinError.set(null);
    factErrors.set({});
    const inh: Partial<Record<BranchFieldName, boolean>> = {};
    if (sec === "taxpayer") {
      inName.set(t.name);
      inTin.set(t.tin_root);
      inType.set(t.taxpayer_type);
    } else if (sec === "branch") {
      inCode.set(b.code);
      inBranchName.set(b.name);
      facts.rdo_code.set(b.facts.rdo_code || "");
      inh.rdo_code = (b.inherited || []).includes("rdo_code");
    } else {
      for (const [f] of ADDRESS_FIELDS) {
        facts[f].set(b.facts[f] || "");
        inh[f] = (b.inherited || []).includes(f);
      }
    }
    inherit.set(inh);
  };
  const setInherit = (f: BranchFieldName, on: boolean) => {
    inherit.update((x) => ({ ...x, [f]: on }));
    if (on) facts[f].set(head()?.facts[f] || "");
  };

  const saveSection = async (sec: "taxpayer" | "branch" | "address") => {
    const t = tp();
    const b = branch();
    if (!t || !b) return;
    const errs: Partial<Record<BranchFieldName, string>> = {};
    const fieldsOf: BranchFieldName[] = sec === "branch" ? ["rdo_code"] : sec === "address" ? ADDRESS_FIELDS.map(([f]) => f) : [];
    for (const f of fieldsOf) {
      if (!b.is_head_office && inherit()[f]) continue;
      const e = factError(f, facts[f](), rdos());
      if (e) errs[f] = e;
    }
    factErrors.set(errs);
    if (Object.keys(errs).length) return;
    if (sec === "taxpayer") {
      const digits = inTin();
      if (digits.length !== 9) return tinError.set("TIN is 9 digits, like 123-456-789.");
      let moved = false;
      // Name and type first, under the TIN they were opened with: a refused
      // move must not leave a moved TIN with the rest unsaved.
      const ok = await write(() => api.updateTaxpayer(t.tin_root, { name: inName().trim() || t.name, taxpayer_type: inType() }));
      if (!ok) return;
      if (digits !== t.tin_root) {
        try {
          await api.moveTaxpayerTin(t.tin_root, digits);
          moved = true;
        } catch (e) {
          tinError.set(`Name and type saved. The TIN did not change: ${errorText(e)}`);
          bump();
          return;
        }
        SCOPE = digits;
        root.set(digits);
      }
      editSec.set(null);
      tinError.set(null);
      say(moved ? `TIN changed to ${formatTin(digits)}. Every branch, tax year, return and template moved with it.` : "Changes saved");
      return;
    }
    if (sec === "branch") {
      let currentTin = b.tin;
      if (!b.is_head_office && inCode() !== b.code) {
        const nc = inCode();
        if (!/^\d{5}$/.test(nc)) return tinError.set("Branch code is 5 digits, like 00001.");
        if (nc === "00000" || t.branches.some((x) => x.code === nc)) return tinError.set(`Branch ${nc} already exists.`);
        try {
          await api.moveBranchCode(b.tin, nc);
        } catch (e) {
          tinError.set(errorText(e));
          return;
        }
        code.set(nc);
        currentTin = t.tin_root + nc;
      }
      tinError.set(null);
      const fields: FieldUpdate = { rdo_code: !b.is_head_office && inherit().rdo_code ? null : facts.rdo_code().trim() };
      if (await write(() => api.updateBranch(currentTin, { name: inBranchName().trim() || b.name, fields }), "Changes saved")) editSec.set(null);
      return;
    }
    const fields: FieldUpdate = {};
    for (const [f] of ADDRESS_FIELDS) fields[f] = !b.is_head_office && inherit()[f] ? null : facts[f]().trim();
    if (await write(() => api.updateBranch(b.tin, { fields }), "Changes saved")) editSec.set(null);
  };

  const selectBranch = (c: string) => {
    code.set(c);
    editSec.set(null);
    tinError.set(null);
    inboxEdit.set(null);
    inboxMsg.set(null);
    formsEdit.set(null);
    addYear.set(false);
    newBranch.set(false);
  };

  // ---------------------------------------------------------------- new branch (renderNewBranch)
  const newBranch = signal(false);
  const nbCode = signal("");
  const nbName = signal("");
  const nbFacts: Record<BranchFieldName, Signal<string>> = {
    rdo_code: signal(""),
    registered_address: signal(""),
    zip_code: signal(""),
    phone: signal(""),
    email: signal(""),
  };
  const nbInherit = signal<Partial<Record<BranchFieldName, boolean>>>({});
  const nbErrors = signal<Record<string, string>>({});
  const nbError = signal<string | null>(null);
  const openNewBranch = async () => {
    const t = tp();
    if (!t) return;
    let suggestion = "";
    try {
      suggestion = await api.suggestNextBranchCode(t.tin_root);
    } catch {}
    const h = head() || t.branches[0];
    nbCode.set(suggestion);
    nbName.set("");
    const inh: Partial<Record<BranchFieldName, boolean>> = {};
    for (const f of Object.keys(nbFacts) as BranchFieldName[]) {
      nbFacts[f].set(h ? h.facts[f] || "" : "");
      inh[f] = Boolean(h);
    }
    nbInherit.set(inh);
    nbErrors.set({});
    nbError.set(null);
    newBranch.set(true);
  };
  const createBranch = async () => {
    const t = tp();
    if (!t) return;
    const c = nbCode();
    const name = nbName().trim();
    const errs: Record<string, string> = {};
    if (!/^\d{5}$/.test(c)) errs.code = "Branch code is 5 digits, like 00001.";
    if (!name) errs.name = "Type a branch name, like Cebu branch.";
    for (const f of Object.keys(nbFacts) as BranchFieldName[]) {
      const e = nbInherit()[f] ? null : factError(f, nbFacts[f](), rdos());
      if (e) errs[f] = e;
    }
    nbErrors.set(errs);
    nbError.set(null);
    if (Object.keys(errs).length) return;
    const fields: FieldUpdate = {};
    for (const f of Object.keys(nbFacts) as BranchFieldName[]) fields[f] = nbInherit()[f] ? null : nbFacts[f]().trim();
    try {
      await api.createBranch(t.tin_root, { branch_code: c, name, fields });
      newBranch.set(false);
      bump();
      selectBranch(c);
      say(`${name} added.`);
    } catch (e) {
      const text = errorText(e);
      if (/branch code/i.test(text)) nbErrors.set({ ...errs, code: text });
      else nbError.set(text);
    }
  };

  // ---------------------------------------------------------------- confirmation inbox (P2-5)
  type InboxEdit = { method: "AppPassword" | "GoogleOAuth"; email: Signal<string>; host: Signal<string>; password: Signal<string>; error: Signal<string | null> };
  const inboxEdit = signal<InboxEdit | null>(null);
  const inboxMsg = signal<{ ok: boolean; text: string } | null>(null);
  const inboxBusy = signal(false);
  const startInboxEdit = (method?: "AppPassword" | "GoogleOAuth") => {
    const s = inbox();
    inboxEdit.set({
      method: method || (s?.auth_method === "GoogleOAuth" ? "GoogleOAuth" : "AppPassword"),
      email: signal(s?.imap_login || ""),
      host: signal(s?.imap_host || "imap.gmail.com"),
      password: signal(""),
      error: signal<string | null>(null),
    });
  };
  const saveInbox = async () => {
    const ib = inboxEdit();
    if (!ib || ib.method !== "AppPassword") return;
    const email = ib.email().trim();
    if (!EMAIL_RE.test(email)) return ib.error.set("Type an email like name@example.com.");
    try {
      await api.setInboxAppPassword(tin(), { imap_email: email, imap_host: ib.host().trim() || null, app_password: ib.password().trim() || null });
      inboxEdit.set(null);
      bump();
      say("Inbox credentials saved. Verify the connection to start tracking.");
    } catch (e) {
      ib.error.set(errorText(e));
    }
  };
  const inboxRun = async (pending: string, fn: () => Promise<{ ok: boolean; text: string }>) => {
    if (inboxBusy()) return;
    inboxBusy.set(true);
    inboxMsg.set({ ok: true, text: pending });
    try {
      const r = await fn();
      inboxMsg.set(r);
      say(r.text, r.ok);
    } catch (e) {
      inboxMsg.set({ ok: false, text: errorText(e) });
    }
    inboxBusy.set(false);
    bump();
  };
  const verifyInbox = () =>
    inboxRun("Testing connection…", async () => {
      const r = await api.testInboxConnection(tin());
      return { ok: r.ok, text: r.message };
    });
  const checkInbox = () =>
    inboxRun("Checking the inbox for BIR confirmations…", async () => {
      const r = await api.checkInboxNow(tin());
      return r.ok
        ? { ok: true, text: r.still_pending ? "Checked. A submitted return is still waiting for its confirmation." : "Checked. Everything submitted is confirmed." }
        : { ok: false, text: r.error || "The check failed." };
    });
  const connectInbox = () =>
    inboxRun("Google sign-in opened in your browser…", async () => {
      await api.connectInboxOauth(tin());
      inboxEdit.set(null);
      return { ok: true, text: "Google account connected. Email tracking is on." };
    });

  // ---------------------------------------------------------------- tax years and forms
  const addYear = signal(false);
  const addYearSel = signal<number | null>(null);
  const addYearCopy = signal(true);
  const allowedYears = computed(() => {
    const { min, max } = bounds();
    const have = years();
    const out: number[] = [];
    for (let y = max; y >= min; y--) if (!have.includes(y)) out.push(y);
    return out;
  });
  const lastYear = computed(() => {
    const ys = years();
    return ys.length ? ys[ys.length - 1] : null;
  });
  const lastYearForms = resource(
    () => !!tin() && lastYear() !== null && { tin: tin(), year: lastYear() as number, v: ver() },
    (s) => api.yearlyForms(s.tin, s.year),
    { initialValue: [] as string[] },
  );
  /** The year the Add band offers: the one after the latest, else this year, else the newest allowed. */
  const addYearPick = computed(() => {
    const allowed = allowedYears();
    const sel = addYearSel();
    if (sel !== null && allowed.includes(sel)) return sel;
    const last = lastYear();
    const now = new Date().getFullYear();
    return last ? (allowed.includes(last + 1) ? last + 1 : allowed[0]) : allowed.includes(now) ? now : allowed[0];
  });
  const addTaxYear = async () => {
    const y = addYearPick();
    if (y === undefined) return;
    try {
      const r = await api.addTaxYear(tin(), y, years().length > 0 && addYearCopy());
      selYear.set(y);
      addYear.set(false);
      formsEdit.set(null);
      bump();
      const n = (r.forms || []).length;
      say(r.copied_from ? `${y} added with ${plural(n, "form")} from ${r.copied_from}.` : `${y} added. Choose the forms it files.`);
    } catch (e) {
      say(errorText(e), false);
    }
  };

  const formsEdit = signal<Set<string> | null>(null);
  const formsQuery = signal("");
  const formsFreq = signal("All");
  const editForms = () => {
    formsEdit.set(new Set(yearForms()));
    formsQuery.set("");
    formsFreq.set("All");
    addYear.set(false);
    tab.set("forms");
  };
  const toggleForm = (c: string, on: boolean) => {
    const next = new Set(formsEdit() || []);
    if (on) next.add(c);
    else next.delete(c);
    formsEdit.set(next);
  };
  const applyPack = (pack: StarterPackInfo) => {
    const next = new Set(formsEdit() || []);
    for (const c of pack.forms) next.add(c);
    formsEdit.set(next);
    say(`${pack.label} forms selected. Save to keep them.`);
  };
  const saveForms = async () => {
    const y = year();
    const after = formsEdit();
    if (y === null || !after) return;
    const before = new Set(yearForms());
    const added = [...after].filter((c) => !before.has(c)).length;
    const removed = [...before].filter((c) => !after.has(c)).length;
    if (await write(() => api.setYearlyForms(tin(), y, [...after]), `${y} forms updated: ${added} added, ${removed} removed.`)) formsEdit.set(null);
  };
  /** Next open period and its due date for a form in the selected year. */
  const schedule = (c: string, y: number) => {
    const rows = yearDeadlines().filter((d) => d.tin === tin() && d.form_code === c && d.taxable_year === y);
    if (!rows.length) return { next: null as string | null, due: "", none: true };
    const openRows = rows.filter((d) => d.status === "Not started" || d.status === "Draft");
    const upcoming = openRows.find((d) => daysUntil(d.due_date) >= 0) || openRows[openRows.length - 1];
    if (!upcoming) return { next: null as string | null, due: "", none: false };
    return { next: upcoming.period_label, due: shortDate(upcoming.due_date), none: false };
  };
  const yearIndex = computed(() => {
    const codes = new Set(yearForms());
    return index().filter((f) => codes.has(f.c));
  });
  const editIndex = computed(() => {
    const q = formsQuery().trim().toLowerCase();
    return index().filter((f) => (formsFreq() === "All" || f.freq === formsFreq()) && (!q || searchText(f).includes(q)));
  });

  // ---------------------------------------------------------------- templates
  const makeTemplate = (c: string) =>
    write(async () => {
      try {
        await api.createTemplateFromLatest(tin(), c);
      } catch (e) {
        const text = errorText(e);
        throw new Error(
          /no field classification/i.test(text)
            ? `Templates for ${c} are not available yet. So far only 1601C and 2551Q returns can keep their setup answers.`
            : /no return|not ?found|no .*draft/i.test(text)
              ? `Open and save a ${c} return first. The template keeps its setup answers.`
              : text,
        );
      }
    }, `${c} template saved from the newest return.`);

  // ---------------------------------------------------------------- archive / delete
  const deleteArmed = signal(false);
  const canRemove = computed(() => {
    const a = auth();
    const t = tp();
    return Boolean(a && t && a.mode && t.tin_root !== a.admin_root);
  });

  // Keep the toast and armed delete from lingering across branch switches.
  effect(() => {
    code();
    deleteArmed.set(false);
  });

  // ---------------------------------------------------------------- views

  const header = () => {
    const fullTin = () => formatTin(tp()?.tin_root || "", branch()?.code);
    return (
      <View class="gap-4">
        <View class="flex-row flex-wrap items-center gap-3">
          <View class={`rounded-full items-center justify-center ${C.accentBg}`} style={{ width: 40, height: 40 }}>
            <Text class="text-sm font-semibold text-white dark:text-white">{initials(tp()?.name || "")}</Text>
          </View>
          <View class="flex-1 gap-0.5" style={{ minWidth: 200 }}>
            <Text class={`text-xl font-semibold ${C.fg}`}>{tp()?.name || ""}</Text>
            <Text class={`text-xs ${C.fg2}`} selectable>
              {`${fullTin()} · ${branch()?.name || ""} · ${tp()?.taxpayer_type || ""}${tp()?.archived ? " · Archived" : ""}`}
            </Text>
          </View>
          <Button
            label="Copy TIN"
            onPress={() => {
              void Clipboard.writeText(fullTin());
              say(`Copied ${fullTin()}`);
            }}
          />
          <Button label="New return" variant="primary" onPress={() => openForms("New return", tin(), root())} />
        </View>
        <Segmented
          options={[
            { value: "profile", label: "Profile" },
            { value: "forms", label: `Forms ${yearForms().length}` },
            { value: "templates", label: `Templates ${templates().length}` },
            { value: "returns", label: `Returns${dues().length ? ` · ${dues().length} due` : ""}` },
          ]}
          value={tab()}
          onChange={(v) => {
            tab.set(v as "profile" | "forms" | "templates" | "returns");
            editSec.set(null);
            formsEdit.set(null);
            addYear.set(false);
          }}
        />
      </View>
    );
  };

  const factInput = (f: BranchFieldName, values: Record<BranchFieldName, Signal<string>>, error: () => string | undefined, disabled: () => boolean) =>
    f === "rdo_code" ? (
      <Combo value={values.rdo_code} options={rdos()} placeholder="Type a code or district, like 050 or Makati" error={!!error()} disabled={disabled()} />
    ) : f === "zip_code" ? (
      <Combo value={values.zip_code} options={zips()} placeholder="Type a code or place, like 1226 or Makati" error={!!error()} disabled={disabled()} />
    ) : (
      <TextInput value={values[f]} error={!!error()} disabled={disabled()} placeholder={f === "email" ? "name@example.com" : undefined} />
    );

  /** One fact of the selected branch: read-only, or its editor with the head-office switch. */
  const factField = (sec: "branch" | "address", f: BranchFieldName, label: string, full: boolean) => {
    const b = () => branch() as Branch;
    const editing = () => editSec() === sec;
    const inheritable = () => !b().is_head_office && !!head();
    return (
      <Field
        label={label}
        full={full}
        value={b().facts[f]}
        inherited={!editing() && inheritable() && (b().inherited || []).includes(f)}
      >
        {editing() ? (
          <View class="gap-1">
            {factInput(f, facts, () => factErrors()[f], () => inheritable() && !!inherit()[f])}
            <ErrorText text={factErrors()[f]} />
            <If when={inheritable()}>
              <Switch on={!!inherit()[f]} onChange={(v) => setInherit(f, v)} label={`${label}: same as head office`} />
            </If>
          </View>
        ) : null}
      </Field>
    );
  };

  const profileTab = () => {
    const ib = () => inbox();
    const inboxMeta = () => {
      const s = ib();
      return s
        ? s.tracking_enabled
          ? `Tracking on · ${s.inbox_email}`
          : s.oauth_connected || s.has_app_password
            ? `Connected · ${s.inbox_email || s.oauth_inbox_email || ""}`
            : "Not connected"
        : "Not connected";
    };
    const agentForms = () => (yearForms().length ? yearForms() : ["2551Q"]);
    const granted = () => new Set(grants().map((g) => g.form));
    const inhCount = () => ADDRESS_FIELDS.filter(([f]) => !branch()?.is_head_office && (branch()?.inherited || []).includes(f)).length;
    const inboxMethod = () => inboxEdit()?.method || ib()?.auth_method || "AppPassword";
    return (
      <View class="gap-3">
        <Section
          title="Taxpayer"
          meta="Shared by every branch"
          open={!!open().taxpayer}
          onToggle={() => toggle("taxpayer")}
          onEdit={() => startEdit("taxpayer")}
          editing={editSec() === "taxpayer"}
        >
          <View class="flex-row flex-wrap gap-4">
            <Field label="Registered name" value={tp()?.name} full>
              {editSec() === "taxpayer" ? <TextInput value={inName} /> : null}
            </Field>
            <Field label="TIN" value={formatTin(tp()?.tin_root || "")}>
              {editSec() === "taxpayer" ? (
                <View class="gap-1">
                  <TextInput value={inTin} digits={9} placeholder="9 digits, like 123456789" error={!!tinError()} />
                  <ErrorText text={tinError()} />
                </View>
              ) : null}
            </Field>
            <Field label="Taxpayer type" value={tp()?.taxpayer_type} full={editSec() === "taxpayer"}>
              {editSec() === "taxpayer" ? (
                <Segmented options={TAXPAYER_TYPES.map((x) => ({ value: x, label: x }))} value={inType()} onChange={(v) => inType.set(v)} />
              ) : null}
            </Field>
          </View>
          <If when={editSec() === "taxpayer"}>
            <Text class={`text-xs ${C.fg3}`}>Changing the TIN moves every branch, tax year, return and template to the new number.</Text>
            <FormActions onCancel={() => editSec.set(null)} onSave={() => saveSection("taxpayer")} busy={busy()} />
          </If>
        </Section>

        <Section
          title="Branch"
          meta={branch()?.is_head_office ? "Head office" : `Branch ${branch()?.code || ""}`}
          open={!!open().branch}
          onToggle={() => toggle("branch")}
          onEdit={() => startEdit("branch")}
          editing={editSec() === "branch"}
        >
          <View class="flex-row flex-wrap gap-4">
            <Field label="Branch code" value={branch()?.code}>
              {editSec() === "branch" && !branch()?.is_head_office ? (
                <View class="gap-1">
                  <TextInput value={inCode} digits={5} placeholder="00001" error={!!tinError()} />
                  <ErrorText text={tinError()} />
                </View>
              ) : null}
            </Field>
            <Field label="Branch name" value={branch()?.name}>
              {editSec() === "branch" ? <TextInput value={inBranchName} /> : null}
            </Field>
            {factField("branch", "rdo_code", "RDO", false)}
          </View>
          <If when={editSec() === "branch"}>
            <FormActions onCancel={() => editSec.set(null)} onSave={() => saveSection("branch")} busy={busy()} />
          </If>
        </Section>

        <Section
          title="Address and contact"
          meta={branch()?.is_head_office ? "Used on every return" : inhCount() ? `${inhCount()} of 4 from head office` : "All set for this branch"}
          open={!!open().address}
          onToggle={() => toggle("address")}
          onEdit={() => startEdit("address")}
          editing={editSec() === "address"}
        >
          <View class="flex-row flex-wrap gap-4">
            <Each of={ADDRESS_FIELDS}>{(a) => factField("address", a()[0], a()[1], a()[2])}</Each>
          </View>
          <If when={editSec() === "address"}>
            <FormActions onCancel={() => editSec.set(null)} onSave={() => saveSection("address")} busy={busy()} />
          </If>
        </Section>

        <Section
          title="Branches"
          meta={plural(tp()?.branches.length || 0, "branch").replace("branchs", "branches")}
          open={!!open().branches}
          onToggle={() => toggle("branches")}
        >
          <ListGroup>
            <Each of={tp()?.branches || []}>
              {(b, i) => (
                <View
                  onPress={() => selectBranch(b().code)}
                  activeStyle={{ opacity: 0.7 }}
                  class={`flex-row items-center gap-3 px-4 py-2 ${i() ? "border-t border-black/5 dark:border-white/5" : ""} ${b().code === branch()?.code ? C.inset : ""}`}
                >
                  <Code text={b().code} />
                  <NameCell title={b().name} sub={b().is_head_office ? "Head office" : b().facts.registered_address || "Same address as head office"} />
                  <If when={b().code === branch()?.code}>
                    <Chip label="Showing" tone="accent" />
                  </If>
                </View>
              )}
            </Each>
          </ListGroup>
          <If
            when={newBranch()}
            else={
              <View class="flex-row">
                <Button label="New branch" onPress={openNewBranch} />
              </View>
            }
          >
            <View class={`rounded-xl p-4 gap-3 ${C.card} ${C.line}`}>
              <Text class={`text-sm font-semibold ${C.fg}`}>New branch</Text>
              <Text class={`text-xs ${C.fg2}`}>
                {`A branch of ${tp()?.name || ""}. It starts with the head office's details; switch any of them off to set its own.`}
              </Text>
              <View class="flex-row flex-wrap gap-4">
                <Field label="Branch code">
                  <View class="gap-1">
                    <TextInput value={nbCode} digits={5} placeholder="00001" error={!!nbErrors().code} />
                    <ErrorText text={nbErrors().code} />
                  </View>
                </Field>
                <Field label="Branch name">
                  <View class="gap-1">
                    <TextInput value={nbName} placeholder="Cebu branch" error={!!nbErrors().name} />
                    <ErrorText text={nbErrors().name} />
                  </View>
                </Field>
                <Each of={[["rdo_code", "RDO", false], ...ADDRESS_FIELDS] as [BranchFieldName, string, boolean][]}>
                  {(a) => (
                    <Field label={a()[1]} full={a()[0] === "registered_address" || a()[0] === "email"}>
                      <View class="gap-1">
                        {factInput(a()[0], nbFacts, () => nbErrors()[a()[0]], () => !!nbInherit()[a()[0]])}
                        <ErrorText text={nbErrors()[a()[0]]} />
                        <Switch
                          on={!!nbInherit()[a()[0]]}
                          onChange={(v) => {
                            nbInherit.update((x) => ({ ...x, [a()[0]]: v }));
                            if (v) nbFacts[a()[0]].set(head()?.facts[a()[0]] || "");
                          }}
                          label={`${a()[1]}: same as head office`}
                        />
                      </View>
                    </Field>
                  )}
                </Each>
              </View>
              <ErrorText text={nbError()} />
              <FormActions onCancel={() => newBranch.set(false)} onSave={createBranch} saveLabel="Add branch" />
            </View>
          </If>
        </Section>

        <Section title="Confirmation inbox" meta={inboxMeta()} open={!!open().inbox} onToggle={() => toggle("inbox")}>
          <LoadError error={inbox.error()} retry={inbox.refetch} what="the inbox" />
          <Field label="Authentication method" full>
            <Segmented
              options={[
                { value: "GoogleOAuth", label: "Google account (OAuth2)" },
                { value: "AppPassword", label: "App password" },
              ]}
              value={inboxMethod()}
              onChange={(v) => {
                const ib0 = inboxEdit();
                if (ib0) inboxEdit.set({ ...ib0, method: v as "AppPassword" | "GoogleOAuth" });
                else startInboxEdit(v as "AppPassword" | "GoogleOAuth");
              }}
            />
          </Field>
          <If
            when={inboxMethod() === "AppPassword"}
            else={
              <Field
                label="Google account"
                full
                value={ib()?.oauth_connected ? `Connected as ${ib()?.oauth_inbox_email || ib()?.inbox_email}` : "Not connected"}
              />
            }
          >
            <View class="flex-row flex-wrap gap-4">
              <Field label="Inbox email" value={ib()?.imap_login || ib()?.inbox_email || "Not set"}>
                {inboxEdit() ? <TextInput value={(inboxEdit() as InboxEdit).email} placeholder="name@gmail.com" /> : null}
              </Field>
              <Field label="IMAP host" value={ib()?.imap_host}>
                {inboxEdit() ? <TextInput value={(inboxEdit() as InboxEdit).host} /> : null}
              </Field>
              <Field label="App password" value={ib()?.has_app_password ? "Stored" : "Not set"} full>
                {inboxEdit() ? (
                  <View class="gap-1">
                    <TextInput
                      value={(inboxEdit() as InboxEdit).password}
                      secure
                      placeholder={ib()?.has_app_password ? "Leave empty to keep the stored password" : "16-letter app password"}
                      error={!!(inboxEdit() as InboxEdit).error()}
                    />
                    <ErrorText text={(inboxEdit() as InboxEdit).error()} />
                  </View>
                ) : null}
              </Field>
            </View>
          </If>
          <If when={ib()?.last_check_at}>
            <Field label="Last check" value={`${shortDate(ib()?.last_check_at)}${ib()?.last_check_status ? ` — ${ib()?.last_check_status}` : ""}`} />
          </If>
          <If when={inboxMethod() === "GoogleOAuth"}>
            <Text class={`text-xs ${C.fg3}`}>
              Profiles that share this inbox share one Google OAuth credential set. Re-authorize or Disconnect updates every profile using this email.
            </Text>
          </If>
          <If when={inboxMethod() === "AppPassword" && inboxEdit()}>
            <Text class={`text-xs ${C.fg3}`}>
              An app password needs 2-Step Verification on the mail account first (Google / Outlook / Yahoo security settings).
            </Text>
          </If>
          <View class="flex-row flex-wrap items-center gap-2">
            <If when={inboxEdit()}>
              <Button label="Cancel" variant="ghost" onPress={() => inboxEdit.set(null)} />
            </If>
            <If when={inboxEdit() && inboxMethod() === "AppPassword"}>
              <Button label="Save" variant="primary" onPress={saveInbox} />
            </If>
            <If when={!inboxEdit() && inboxMethod() === "AppPassword"}>
              <Button label="Edit credentials" variant="ghost" onPress={() => startInboxEdit("AppPassword")} />
            </If>
            <If when={inboxMethod() === "GoogleOAuth"}>
              <Button label={ib()?.oauth_connected ? "Re-authorize" : "Connect Google account"} disabled={inboxBusy()} onPress={connectInbox} />
            </If>
            <If when={inboxMethod() === "GoogleOAuth" && ib()?.oauth_connected}>
              <Button
                label="Disconnect"
                variant="ghost"
                onPress={() => write(() => api.disconnectInboxOauth(tin()), "Google account disconnected for every profile sharing this inbox.")}
              />
            </If>
            <Button label="Verify connection" disabled={inboxBusy()} onPress={verifyInbox} />
            <Button label="Check now" variant="ghost" disabled={inboxBusy()} onPress={checkInbox} />
          </View>
          <If when={inboxMsg()}>
            <Text class={`text-sm ${inboxMsg()?.ok ? C.success : C.danger}`}>{inboxMsg()?.text}</Text>
          </If>
          <View class="flex-row flex-wrap items-center gap-3">
            <Switch
              on={!!ib()?.tracking_enabled}
              onChange={(on) => write(() => api.setInboxTracking(tin(), on), on ? "Automated receipt tracking on." : "Automated receipt tracking off.")}
              label="Automated receipt tracking"
            />
          </View>
          <Text class={`text-xs ${ib()?.tracking_enabled ? C.success : C.fg3}`}>
            {ib()?.tracking_enabled
              ? ib()?.still_pending
                ? "● Automated BIR receipt tracking is active — a submitted return is still waiting for its confirmation."
                : "● Automated BIR receipt tracking is active."
              : "○ Verify your connection to activate email tracking."}
          </Text>
        </Section>

        <Section
          title="Agent filing"
          meta={grants().length ? grants().map((g) => g.form).join(", ") : "Assistant must ask before filing"}
          open={!!open().agent}
          onToggle={() => toggle("agent")}
        >
          <LoadError error={grants.error()} retry={grants.refetch} what="the standing OKs" />
          <Text class={`text-xs ${C.fg2}`}>
            A standing OK lets an assistant file that form for this branch without asking each time. The Queue button does not need it. You can turn it off here.
          </Text>
          <Each of={agentForms()}>
            {(c) => (
              <Switch
                on={granted().has(c())}
                onChange={(next) =>
                  write(
                    () => (next ? api.grantSend(tin(), c()) : api.revokeSend(tin(), c())),
                    next ? `Assistant may file ${c()}.` : `Assistant may no longer file ${c()}.`,
                  )
                }
                label={c()}
              />
            )}
          </Each>
        </Section>

        <If when={canRemove()}>
          <SectionTitle text="Archive or delete" />
          <ListGroup>
            <Row first>
              <NameCell
                title={tp()?.archived ? "Archived" : "Archive this taxpayer"}
                sub={tp()?.archived ? "Kept with its returns, out of new work. Unarchive to file for it again" : "Keeps every return; it just leaves new work"}
              />
              <Button
                label={tp()?.archived ? "Unarchive" : "Archive"}
                onPress={() => {
                  const t = tp() as Taxpayer;
                  write(() => api.setTaxpayerArchived(t.tin_root, !t.archived), t.archived ? `${t.name} is back in new work.` : `${t.name} archived.`);
                }}
              />
            </Row>
            <Row>
              <NameCell
                title="Delete this taxpayer"
                sub={deleteArmed() ? "Every branch and return goes. This cannot be undone" : "Removes it and every branch and return"}
              />
              <If when={deleteArmed()}>
                <Button label="Keep it" variant="ghost" onPress={() => deleteArmed.set(false)} />
              </If>
              <Button
                label={deleteArmed() ? "Delete for good" : "Delete"}
                variant="danger"
                onPress={async () => {
                  if (!deleteArmed()) return deleteArmed.set(true);
                  const t = tp() as Taxpayer;
                  try {
                    await api.deleteTaxpayer(t.tin_root);
                    deleted.set(t.name);
                  } catch (e) {
                    say(errorText(e), false);
                  }
                }}
              />
            </Row>
          </ListGroup>
        </If>
      </View>
    );
  };

  const addYearBand = () => (
    <View class={`flex-row flex-wrap items-center gap-3 rounded-xl px-4 py-3 ${C.card} ${C.line}`}>
      <If
        when={allowedYears().length}
        else={
          <Text class={`text-sm flex-1 ${C.fg}`}>{`Every tax year from ${bounds().min} to ${bounds().max} is already added.`}</Text>
        }
      >
        <Text class={`text-sm font-semibold ${C.fg}`}>Add tax year</Text>
        <Segmented
          options={allowedYears()
            .slice(0, 8)
            .map((y) => ({ value: y, label: String(y) }))}
          value={addYearPick() ?? null}
          onChange={(v) => addYearSel.set(v)}
        />
        <If when={lastYear()}>
          <Checkbox on={addYearCopy()} onChange={(v) => addYearCopy.set(v)} label={`Copy ${plural(lastYearForms().length, "form")} from ${lastYear()}`} />
        </If>
        <View class="flex-1" />
      </If>
      <If when={years().length}>
        <Button label="Cancel" variant="ghost" onPress={() => addYear.set(false)} />
      </If>
      <If when={allowedYears().length}>
        <Button label={`Add ${addYearPick() ?? ""}`} variant="primary" onPress={addTaxYear} />
      </If>
    </View>
  );

  const formRow = (f: ScreenFormIndexEntry, first: boolean) => {
    const y = () => year() as number;
    const on = () => !formsEdit() || (formsEdit() as Set<string>).has(f.c);
    const tpl = () => templates().some((x) => x.form_code === f.c);
    const sc = () => (on() ? schedule(f.c, y()) : { next: null, due: "", none: false });
    const sub = () => (formsEdit() && !on() ? "Not filed this year" : sc().next ? `Next: ${sc().next}` : sc().none ? `Filed in ${y()}` : "Every period filed");
    return (
      <Row first={first}>
        <If when={formsEdit()}>
          <Checkbox on={on()} onChange={(v) => toggleForm(f.c, v)} />
        </If>
        <Code text={f.c} dim={!on()} />
        <NameCell title={f.t} sub={sub()} dim={!on()} />
        <Chip label={f.freq} />
        <Text class={`text-xs ${C.fg2}`} style={{ width: 104 }}>{on() && sc().due ? `Due ${sc().due}` : ""}</Text>
        <View style={{ width: 104 }}>
          <If when={on()}>
            <Chip label={tpl() ? "Template" : "No template"} tone={tpl() ? "success" : "plain"} />
          </If>
        </View>
      </Row>
    );
  };

  const formsTab = () => (
    <View class="gap-4">
      <LoadError error={years.error()} retry={years.refetch} what="the tax years" />
      <LoadError error={yearForms.error()} retry={yearForms.refetch} what="the forms" />
      <LoadError error={index.error()} retry={index.refetch} what="the form list" />
      <If
        when={years().length}
        else={
          <View class="gap-4">
            <Text class={`text-sm ${C.fg2}`}>{`${branch()?.name || ""} has no tax year yet. Add one to choose the forms it files.`}</Text>
            {addYearBand()}
          </View>
        }
      >
        <View class="flex-row flex-wrap items-center gap-3">
          <Segmented
            options={years().map((y) => ({ value: y, label: String(y) }))}
            value={year()}
            onChange={(v) => {
              selYear.set(v);
              formsEdit.set(null);
              addYear.set(false);
            }}
          />
          <Button
            label="Add year"
            variant="ghost"
            onPress={() => {
              addYear.set(!addYear());
              addYearSel.set(null);
              addYearCopy.set(true);
              formsEdit.set(null);
            }}
          />
          <View class="flex-1" />
          <If when={formsEdit()} else={<Button label="Edit forms" onPress={editForms} />}>
            <Text class={`text-xs ${C.fg2}`}>{`${formsEdit()?.size || 0} selected`}</Text>
            <Button label="Cancel" variant="ghost" onPress={() => formsEdit.set(null)} />
            <Button label="Save forms" variant="primary" onPress={saveForms} disabled={busy()} />
          </If>
        </View>
        <If when={addYear()}>{addYearBand()}</If>
        <If
          when={formsEdit()}
          else={
            <ListGroup>
              <If
                when={yearIndex().length}
                else={
                  <EmptyRow text={`No forms for ${year()} yet.`}>
                    <Button label="Choose forms" onPress={editForms} />
                  </EmptyRow>
                }
              >
                <Each of={yearIndex()}>{(f, i) => formRow(f(), i() === 0)}</Each>
              </If>
            </ListGroup>
          }
        >
          <View class="flex-row flex-wrap items-center gap-3">
            <View style={{ width: 280 }}>
              <TextInput value={formsQuery} placeholder="Find a form, like 1601C or VAT" />
            </View>
            <Segmented options={FREQS.map((x) => ({ value: x, label: x }))} value={formsFreq()} onChange={(v) => formsFreq.set(v)} />
          </View>
          <View class="flex-row flex-wrap items-center gap-2">
            <Text class={`text-xs ${C.fg3}`}>Starter packs</Text>
            <Each of={packs()}>{(pack) => <Button label={pack().label} onPress={() => applyPack(pack())} />}</Each>
          </View>
          <ListGroup>
            <If when={editIndex().length} else={<EmptyRow text="No form matches. Try a code like 1601C or a word like VAT." />}>
              <Each of={editIndex()}>{(f, i) => formRow(f(), i() === 0)}</Each>
            </If>
          </ListGroup>
        </If>
      </If>
    </View>
  );

  const templatesTab = () => (
    <View class="gap-4">
      <LoadError error={templates.error()} retry={templates.refetch} what="the templates" />
      <If
        when={years().length}
        else={
          <ListGroup>
            <EmptyRow text={`${branch()?.name || ""} has no tax year yet. Add one to choose the forms it files.`}>
              <Button
                label="Add year"
                onPress={() => {
                  addYear.set(true);
                  tab.set("forms");
                }}
              />
            </EmptyRow>
          </ListGroup>
        }
      >
        <Text class={`text-sm ${C.fg2}`}>
          {`One template per form for ${branch()?.name || ""}, kept across tax years. These are the forms it files in ${year()}. A template keeps setup answers and never keeps amounts.`}
        </Text>
        <ListGroup>
          <If
            when={yearIndex().length}
            else={
              <EmptyRow text={`No forms for ${year()} yet.`}>
                <Button label="Choose forms" onPress={editForms} />
              </EmptyRow>
            }
          >
            <Each of={yearIndex()}>
              {(f, i) => {
                const tpl = () => templates().find((x) => x.form_code === f().c);
                return (
                  <Row first={i() === 0}>
                    <Code text={f().c} dim={!tpl()} />
                    <NameCell title={f().t} sub={tpl() ? `Updated ${shortDate(tpl()?.updated_at)}` : "No template yet"} dim={!tpl()} />
                    <Text class={`text-xs ${C.fg2}`} style={{ width: 128 }}>{tpl() ? `${tpl()?.answers_kept} answers kept` : ""}</Text>
                    <If when={tpl()} else={<Button label="Create" onPress={() => makeTemplate(f().c)} />}>
                      <Button label="Update" variant="ghost" onPress={() => makeTemplate(f().c)} />
                      <Button
                        label="Delete"
                        variant="ghost"
                        onPress={() => write(() => api.deleteFormTemplate(tin(), f().c), `${f().c} template deleted.`)}
                      />
                    </If>
                  </Row>
                );
              }}
            </Each>
          </If>
        </ListGroup>
      </If>
    </View>
  );

  const whoLine = (r: { branch_count: number; branch_name: string; taxpayer_name: string }) => (r.branch_count > 1 ? r.branch_name : r.taxpayer_name);

  const dueRow = (d: DeadlineRow, first: boolean) => {
    const n = daysUntil(d.due_date);
    const [, m, day] = d.due_date.split("-").map(Number);
    const openish = d.status === "Not started" || d.status === "Draft";
    return (
      <Row first={first}>
        <View class="items-center" style={{ width: 44 }}>
          <Text class={`text-xs font-semibold ${C.fg3}`}>{MONTHS[m - 1].toUpperCase()}</Text>
          <Text class={`text-lg font-semibold ${C.fg}`}>{String(day)}</Text>
        </View>
        <Code text={d.form_code} />
        <NameCell title={`${d.form_code} · ${d.period_label}`} sub={whoLine(d)} />
        <View style={{ width: 110 }}>
          {openish && n < 0 ? (
            <Chip label={`${plural(-n, "day")} overdue`} tone="danger" />
          ) : openish && n <= 14 ? (
            <Chip label={n === 0 ? "Due today" : `In ${plural(n, "day")}`} tone="warning" />
          ) : (
            <Text class={`text-xs ${C.fg2}`}>{n === 0 ? "Today" : n < 0 ? `${-n} days ago` : `In ${n} days`}</Text>
          )}
        </View>
        <StatusChip status={d.status} />
        <Button label="Open in Forms" variant="ghost" onPress={() => openForms(`${d.form_code} · ${d.period_label}`, d.tin, d.tin_root, { form_code: d.form_code, year: d.taxable_year, period: d.period, key: d.return_key })} />
      </Row>
    );
  };

  const returnRow = (r: ReturnRow, first: boolean) => {
    return (
      <Row first={first}>
        <Code text={r.form_code} />
        <NameCell title={`${r.form_code} · ${r.period_label}`} sub={whoLine(r)} />
        <Text class={`text-xs ${C.fg2}`} style={{ width: 104 }}>{shortDate(r.updated_at)}</Text>
        <If when={r.status === "Queued"}>
          <Button
            label="Cancel queue"
            variant="ghost"
            onPress={() => write(() => api.cancelQueued(r.tin, r.form_code, r.taxable_year, r.period || null), `${r.form_code} · ${r.period_label} is a draft again.`)}
          />
        </If>
        <StatusChip status={r.status} />
        <Button label="Open in Forms" variant="ghost" onPress={() => openForms(`${r.form_code} · ${r.period_label}`, r.tin, r.tin_root, { form_code: r.form_code, year: r.taxable_year, period: r.period, key: r.return_key })} />
      </Row>
    );
  };

  const returnsTab = () => {
    const drafts = () => returns().filter((r) => statusKind(r.status) === "draft");
    const queued = () => returns().filter((r) => statusKind(r.status) === "queued");
    const filed = () => returns().filter((r) => statusKind(r.status) === "filed");
    const group = (title: string, rows: () => ReturnRow[], empty: string) => (
      <View>
        <SectionTitle text={title} />
        <ListGroup>
          <If when={rows().length} else={<EmptyRow text={empty} />}>
            <Each of={rows()}>{(r, i) => returnRow(r(), i() === 0)}</Each>
          </If>
        </ListGroup>
      </View>
    );
    return (
      <View class="gap-2">
        <Text class={`text-sm ${C.fg2}`}>
          {dues().length
            ? `${dues().length === 1 ? "1 return is" : `${dues().length} returns are`} due in the next 30 days for ${tp()?.name || ""}.`
            : `Nothing is due in the next 30 days for ${tp()?.name || ""}.`}
        </Text>
        <LoadError error={dues.error()} retry={dues.refetch} what="the deadlines" />
        <LoadError error={returns.error()} retry={returns.refetch} what="the returns" />
        <LoadError error={asNeeded.error()} retry={asNeeded.refetch} what="the as-needed forms" />
        <SectionTitle text="Due in the next 30 days" />
        <ListGroup>
          <If when={dues().length} else={<EmptyRow text="Deadlines show here once this taxpayer has forms for a tax year." />}>
            <Each of={dues()}>{(d, i) => dueRow(d(), i() === 0)}</Each>
          </If>
        </ListGroup>
        <If when={asNeeded().length}>
          <SectionTitle text="As needed" />
          <ListGroup>
            <Each of={asNeeded()}>
              {(f, i) => (
                <View>
                  <Row first={i() === 0}>
                    <Code text={f().form_code} />
                    <NameCell title={`${f().form_code} · ${f().form_name}`} sub={`${whoLine(f())} · ${f().due_rule}`} />
                    <Button label="Start a return" onPress={() => openForms(`${f().form_code} · new return`, f().tin, f().tin_root, { form_code: f().form_code, year: f().taxable_year })} />
                  </Row>
                  <Each of={f().undated}>
                    {(r) => (
                      <Row>
                        <View style={{ width: 68 }} />
                        <NameCell
                          title={f().trigger ? `No ${String(f().trigger).toLowerCase()} date yet` : "Saved return"}
                          sub={`Saved ${shortDate(r().updated_at)}`}
                        />
                        <StatusChip status={r().status} />
                        <Button label="Open in Forms" variant="ghost" onPress={() => openForms(`${f().form_code} · saved return`, f().tin, f().tin_root, { form_code: f().form_code, year: f().taxable_year, period: r().period, key: r().return_key })} />
                      </Row>
                    )}
                  </Each>
                </View>
              )}
            </Each>
          </ListGroup>
        </If>
        {group("Drafts", drafts, "Saved drafts show here.")}
        {group("Queued", queued, "Returns queued for filing show here.")}
        {group("Filed", filed, "Filed returns show here.")}
      </View>
    );
  };

  return (
    <View class={`flex-1 ${C.pane}`} style={{ minHeight: 480 }}>
      <If when={toast()}>
        <View
          class={`flex-row items-center gap-3 px-4 py-2 border-b border-black/10 dark:border-white/10 ${toast()?.ok ? C.card : "bg-[#d92d200d] dark:bg-[#ff69611a]"}`}
        >
          <Text class={`text-sm flex-1 ${toast()?.ok ? C.fg : C.danger}`}>{toast()?.text}</Text>
          <Button label="Dismiss" variant="ghost" onPress={() => toast.set(null)} />
        </View>
      </If>
      <If when={handoff()}>
        <View class={`gap-2 px-4 py-3 border-b border-black/10 dark:border-white/10 bg-[#1680f30d] dark:bg-[#1884fc1a]`}>
          <View class="flex-row items-center gap-3">
            <Text class={`text-sm font-semibold flex-1 ${C.fg}`}>{`Open in BIR Forms: ${handoff()?.title || ""}`}</Text>
            <Button
              label="Copy link"
              onPress={() => {
                void Clipboard.writeText(handoff()?.url || "");
                say("Link copied.");
              }}
            />
            <Button label="Close" variant="ghost" onPress={() => handoff.set(null)} />
          </View>
          <Text class={`text-xs ${C.fg2}`}>The link is copied. Open it in Rox to continue in the Forms app:</Text>
          <Text class={`text-xs ${C.accent}`} selectable>
            {handoff()?.url || ""}
          </Text>
        </View>
      </If>
      <Scroll class="flex-1">
        {/* Rox's nsh ignores contentContainerClass: the padding and centering live here */}
        <View class="w-full items-center px-6 py-6">
        <View class="gap-5" style={{ width: "100%", maxWidth: 960 }}>
          <If when={deleted()}>
            <View class={`rounded-xl p-6 gap-2 ${C.card} ${C.line}`}>
              <Text class={`text-base font-semibold ${C.fg}`}>{`${deleted() || ""} deleted.`}</Text>
              <Text class={`text-sm ${C.fg2}`}>Every branch and return went with it. Close this app.</Text>
            </View>
          </If>
          <If when={!deleted()}>
            <LoadError error={tp.error()} retry={tp.refetch} what="the taxpayer" />
            <If when={tp.loading() && !tp()}>
              <Text class={`text-sm ${C.fg2}`}>Loading the taxpayer…</Text>
            </If>
            <If when={!tp.loading() && !tp.error() && tp() === null}>
              <View class={`rounded-xl p-6 gap-2 ${C.card} ${C.line}`}>
                <Text class={`text-base font-semibold ${C.fg}`}>{`No taxpayer ${formatTin(root())}`}</Text>
                <Text class={`text-sm ${C.fg2}`}>It may have been deleted or its TIN changed. Open this from a taxpayer again.</Text>
                <View class="flex-row">
                  <Button label="Retry" onPress={tp.refetch} />
                </View>
              </View>
            </If>
            <If when={tp() && branch()}>
              {header()}
              <If when={tab() === "profile"}>{profileTab()}</If>
              <If when={tab() === "forms"}>{formsTab()}</If>
              <If when={tab() === "templates"}>{templatesTab()}</If>
              <If when={tab() === "returns"}>{returnsTab()}</If>
            </If>
          </If>
        </View>
        </View>
      </Scroll>
    </View>
  );
}
