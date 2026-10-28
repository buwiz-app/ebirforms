// Buwiz Sign-in — onboarding for a new BIR user as a Rox mini app.
//
// Port of bir-shell's first-run path: the Buwiz account gate (app/shell/gate.js,
// account.js; main.js finishUnlock), the "Who will use eBIRForms on this
// computer?" setup (app/shell/auth.js renderSetup; main.js chooseSolo /
// chooseHelpdesk / submitHelpdeskPin / finishHelpdeskSetup), then the first
// taxpayer (app/shell/taxpayer.js renderNewTaxpayer / createTaxpayer). It ends
// with a button that opens the BIR Tax Profile mini app on the new taxpayer's
// ticket (Rox's bridge, `rox.open`: the app never builds a link with a TIN).
//
// Sign-in screens for an existing setup (TIN + PIN), PIN/TOTP management and
// setup changes (security.js, setup_change.js) are not part of onboarding: the
// only code first-run setup requires is the help desk's admin PIN.

import { signal, computed, effect, resource, cleanup, Text, View, Scroll, Input, Overlay } from "nsh";
import type { Signal, JSX, YNode } from "nsh";
import { rox } from "rox";

/** An `<Input>`'s text from its `onChange`: `{ value }` since nsh's events
 *  became objects (#558), the text itself before. */
const typed = (e: { value: string } | string): string => (typeof e === "string" ? e : e.value);

// ============================================================ host calls
// Every bir.* call this app makes goes through `bir` and is listed in `api`:
// Rox's bridge to the BIR plugin's named tool for that host function
// (`bir_app_<fn>`). Onboarding names no taxpayer yet, so it holds no ticket.

async function bir<T = any>(fn: string, ...args: unknown[]): Promise<T> {
  const r: any = await rox.call(`bir_app_${fn}`, { args });
  return r?.result as T;
}

interface CloudAccountStatus {
  server_url: string;
  server_url_source: string;
  signed_in: boolean;
  email: string | null;
  user_id: string | null;
  offline: boolean;
  detail: string;
}
interface CloudSignInStart {
  user_code: string;
  verification_uri: string;
  verification_uri_complete: string;
  browser_opened: boolean;
}
interface AuthState {
  mode: "solo" | "helpdesk" | null;
  admin_root: string | null;
  has_admin_pin: boolean;
  has_admin_totp: boolean;
  has_recovery_key: boolean;
  mac_login_available: boolean;
}
type TaxpayerType = "Individual" | "Corporation" | "Partnership" | "Cooperative" | "Estate" | "Trust";
interface Taxpayer {
  /** BIR's ticket for the taxpayer: what opens its profile. */
  ticket?: string;
  tin_root: string;
  tin_display: string;
  name: string;
  taxpayer_type: TaxpayerType;
  birth_date: string | null;
  needs_grouping_review: boolean;
  archived: boolean;
  branches: { tin: string; code: string; name: string; is_head_office: boolean }[];
}
interface NewTaxpayerInput {
  tin_root: string;
  name: string;
  taxpayer_type: TaxpayerType;
  birth_date?: string | null;
  head_office: { rdo_code?: string; registered_address?: string; zip_code?: string; phone?: string; email?: string };
}
interface RdoOption { code: string; description: string }

const api = {
  cloudStatus: () => bir<CloudAccountStatus>("cloud_account_status"),
  beginSignIn: () => bir<CloudSignInStart>("cloud_account_begin_sign_in"),
  beginSignUp: () => bir<CloudSignInStart>("cloud_account_begin_sign_up"),
  /** Waits until the browser approves this computer. */
  finishSignIn: () => bir<CloudAccountStatus>("cloud_account_finish_sign_in"),
  /** Opens the database for this session; every other call fails until it ran. */
  sessionOpened: () => bir<null>("cloud_account_session_opened"),
  authState: () => bir<AuthState>("auth_state"),
  setupSolo: (root: string) => bir<null>("setup_solo", root),
  setupHelpdesk: (pin: string) => bir<{ recovery_key: string }>("setup_helpdesk", pin),
  listTaxpayers: () => bir<Taxpayer[]>("list_taxpayers"),
  createTaxpayer: (input: NewTaxpayerInput) => bir<Taxpayer>("create_taxpayer", input),
  listRdos: () => bir<RdoOption[]>("list_rdos"),
  listZipCodes: () => bir<string[]>("list_zip_codes"),
};

// ============================================================ helpers

const TAXPAYER_TYPES: TaxpayerType[] = ["Individual", "Corporation", "Partnership", "Cooperative", "Estate", "Trust"];
const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
type Option = { value: string; label: string };

function errorText(e: unknown): string {
  return String(e && (e as any).message ? (e as any).message : e)
    .replace(/^Error:\s*/, "")
    .replace(/^`[^`]+`:\s*/, "");
}
function formatTin(root: string): string {
  const r = String(root || "").replace(/\D/g, "");
  return r.length >= 9 ? `${r.slice(0, 3)}-${r.slice(3, 6)}-${r.slice(6, 9)}` : r;
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
};

type Variant = "primary" | "default" | "ghost";
function Button(p: { label: string; onPress: () => void; variant?: Variant; disabled?: boolean; block?: boolean }) {
  const box = () => (p.variant === "primary" ? C.accentBg : p.variant === "ghost" ? "" : `${C.card} ${C.line}`);
  const ink = () => (p.variant === "primary" ? "text-white dark:text-white" : p.variant === "ghost" ? C.accent : C.fg);
  return (
    <View
      onPress={() => !p.disabled && p.onPress()}
      activeStyle={{ opacity: 0.7 }}
      class={`px-4 py-2 rounded-md items-center justify-center ${box()} ${p.disabled ? "opacity-50" : ""} ${p.block ? "" : "self-start"}`}
      style={{ minHeight: 34 }}
    >
      <Text class={`text-sm font-semibold ${ink()}`}>{p.label}</Text>
    </View>
  );
}


function ErrorText(p: { text: string | null | undefined }) {
  return (
    <If when={p.text}>
      <Text class={`text-xs ${C.danger}`}>{p.text}</Text>
    </If>
  );
}

function Field(p: { label: string; full?: boolean; error?: string | null; children?: JSX.Element }) {
  return (
    <View class="gap-1" style={{ flexGrow: 1, flexBasis: p.full ? "100%" : 220, minWidth: 200 }}>
      <Text class={`text-xs ${C.fg2}`}>{p.label}</Text>
      {p.children}
      <ErrorText text={p.error} />
    </View>
  );
}

function TextInput(p: {
  value: Signal<string>;
  placeholder?: string;
  error?: boolean;
  secure?: boolean;
  digits?: number;
  /** Phone numbers: digits, +, spaces and dashes only. */
  phone?: boolean;
  onBlur?: () => void;
  onEdit?: () => void;
  big?: boolean;
}) {
  return (
    <Input
      value={p.value()}
      onChange={(e) => {
        const v = typed(e);
        p.value.set(p.digits ? v.replace(/\D/g, "").slice(0, p.digits) : p.phone ? v.replace(/[^\d+\s-]/g, "").slice(0, 20) : v);
        p.onEdit?.();
      }}
      onBlur={p.onBlur}
      placeholder={p.placeholder}
      masked={p.secure}
      keyboardType={p.digits ? "number-pad" : "default"}
      class={`placeholder:text-[#8e8e93] rounded-md px-3 py-2 ${p.big ? "text-lg text-center" : "text-sm"} ${C.fg} ${C.pane} border ${p.error ? "border-[#d92d20] dark:border-[#ff6961]" : "border-black/10 dark:border-white/10"}`}
    />
  );
}

/**
 * The TIN as eBIRForms takes it: three boxes of three digits, [123]-[456]-[789].
 * A full box moves on to the next, Backspace in an empty box goes back, and a
 * pasted TIN fills the boxes from the one it lands in. Only digits are kept.
 */
function TinBoxes(p: { value: Signal<string>; error?: boolean; onBlur?: () => void; onEdit?: () => void }) {
  const boxes: (YNode | undefined)[] = [];
  const split = (v: string) => [v.slice(0, 3), v.slice(3, 6), v.slice(6, 9)];
  // The boxes keep their own digits (a short first box must not shift the
  // others); the TIN is them joined, and a reset from outside splits again.
  const parts = signal(split(p.value()));
  effect(() => {
    const v = p.value();
    if (v !== parts().join("")) parts.set(split(v));
  });
  const put = (i: number, raw: string) => {
    const digits = raw.replace(/\D/g, "");
    const next = [...parts()];
    if (digits.length > 3) {
      // a paste: it runs on into the boxes after this one
      const run = (next.slice(0, i).join("") + digits).slice(0, 9);
      const filled = split(run);
      for (let k = i; k < 3; k++) next[k] = filled[k];
      parts.set(next);
      p.value.set(next.join(""));
      p.onEdit?.();
      boxes[Math.min(2, Math.floor(run.length / 3))]?.focus();
      return;
    }
    next[i] = digits;
    parts.set(next);
    p.value.set(next.join(""));
    p.onEdit?.();
    if (digits.length === 3 && i < 2) boxes[i + 1]?.focus();
  };
  // Moving between the boxes is not leaving the TIN: its blur waits a beat
  // and is dropped when another box took the focus.
  let inside = 0;
  const left = () => {
    inside = Math.max(0, inside - 1);
    setTimeout(() => {
      if (!inside) p.onBlur?.();
    }, 60);
  };
  const box = (i: number) => (
    <Input
      onFocus={() => inside++}
      ref={(n) => (boxes[i] = n)}
      value={parts()[i]}
      onChange={(e) => put(i, typed(e))}
      onKeyPress={(e) => {
        if (e.key === "Backspace" && !parts()[i] && i > 0) boxes[i - 1]?.focus();
      }}
      onBlur={left}
      placeholder="000"
      keyboardType="number-pad"
      class={`placeholder:text-[#8e8e93] rounded-md px-2 py-2 text-base text-center tracking-widest ${C.fg} ${C.pane} border ${p.error ? "border-[#d92d20] dark:border-[#ff6961]" : "border-black/10 dark:border-white/10"}`}
      style={{ width: 72 }}
    />
  );
  return (
    <View class="flex-row items-center gap-2">
      {box(0)}
      <Text class={`text-base ${C.fg3}`}>-</Text>
      {box(1)}
      <Text class={`text-base ${C.fg3}`}>-</Text>
      {box(2)}
    </View>
  );
}

/**
 * A type-to-filter pick list, as bir-shell's ComboField: the text box shows the
 * picked label and is itself the search. Up to `max` options (5) float below
 * it: codes that start with the text first, then labels holding every typed
 * word, so "2200", "olongapo" and "050 makati" all find theirs. Up and Down
 * move, Return picks, Escape or a press outside closes. Editing a picked
 * label (Backspace, Command-Delete) clears the pick.
 */
function Combo(p: {
  value: Signal<string>;
  options: Option[];
  placeholder: string;
  error?: boolean;
  max?: number;
  onPick?: () => void;
  onBlur?: () => void;
}) {
  const max = p.max ?? 5;
  let anchor: YNode | undefined;
  let input: YNode | undefined;
  const width = signal(320);
  const open = signal(false);
  const at = signal(0);
  const labelOf = (v: string) => p.options.find((o) => o.value === v)?.label ?? v;
  const text = signal(labelOf(p.value()));
  // The value can change from outside (a reset, the options arriving).
  effect(() => {
    const label = labelOf(p.value());
    if (label !== text() && (p.value() || !open())) text.set(label);
  });
  const picked = computed(() => !!p.value() && p.options.some((o) => o.value === p.value()));
  const lower = computed(() => p.options.map((o) => o.label.toLowerCase()));
  const matches = computed(() => {
    const t = text().trim().toLowerCase();
    if (!t || (picked() && t === labelOf(p.value()).toLowerCase())) return p.options.slice(0, max);
    const words = t.split(/\s+/);
    const first: Option[] = [];
    const rest: Option[] = [];
    p.options.forEach((o, i) => {
      if (o.value.toLowerCase().startsWith(t)) first.push(o);
      else if (words.every((w) => lower()[i].includes(w))) rest.push(o);
    });
    return [...first, ...rest].slice(0, max);
  });
  const pick = (o: Option) => {
    p.value.set(o.value);
    text.set(o.label);
    open.set(false);
    p.onPick?.();
  };
  const edit = (v: string) => {
    // a picked label that loses a character is cleared whole
    if (picked() && v.length < text().length && text() === labelOf(p.value())) {
      p.value.set("");
      text.set("");
    } else {
      text.set(v);
      const exact = p.options.find((o) => o.value === v.trim() || o.label.toLowerCase() === v.trim().toLowerCase());
      p.value.set(exact ? exact.value : v.trim());
    }
    at.set(0);
    open.set(true);
  };
  return (
    <View ref={(n) => (anchor = n)} onLayout={(e: any) => width.set((e.bounds ?? e).size.width)}>
      <View class={`flex-row items-center rounded-md border ${C.pane} ${p.error ? "border-[#d92d20] dark:border-[#ff6961]" : "border-black/10 dark:border-white/10"}`}>
        <Input
          ref={(n) => (input = n)}
          value={text()}
          onChange={(e) => edit(typed(e))}
          onFocus={() => open.set(true)}
          onBlur={() => {
            setTimeout(() => open.set(false), 150);
            p.onBlur?.();
          }}
          onKeyPress={(e) => {
            const n = matches().length;
            if (e.key === "ArrowDown") {
              open.set(true);
              at.set(Math.min(at() + 1, Math.max(0, n - 1)));
            } else if (e.key === "ArrowUp") at.set(Math.max(0, at() - 1));
            else if (e.key === "Escape") open.set(false);
          }}
          onSubmit={() => {
            const o = matches()[at()];
            if (open() && o) pick(o);
          }}
          submitBehavior="submit"
          placeholder={p.placeholder}
          class={`placeholder:text-[#8e8e93] flex-1 px-3 py-2 text-sm ${C.fg}`}
        />
        <View onPress={() => (open() ? open.set(false) : input?.focus())} class="px-3 py-2" activeStyle={{ opacity: 0.6 }}>
          <Text class={`text-xs ${C.fg3}`}>{open() ? "▴" : "▾"}</Text>
        </View>
      </View>
      <If when={open()}>
        <Overlay anchor={anchor} placement="bottom" align="start" offset={4} onPressOutside={() => open.set(false)}>
          <View class={`rounded-md p-1 gap-0.5 ${C.card} border border-black/10 dark:border-white/10 shadow-lg`} style={{ width: width() }}>
            <Each of={matches()}>
              {(o, i) => (
                <View
                  onPress={() => pick(o())}
                  onHoverIn={() => at.set(i())}
                  activeStyle={{ opacity: 0.6 }}
                  class={`px-2 py-1.5 rounded ${i() === at() ? "bg-[#1680f3]/15 dark:bg-[#1884fc]/25" : ""}`}
                >
                  <Text class={`text-sm ${o().value === p.value() ? "font-semibold" : ""} ${C.fg}`} numberOfLines={1}>
                    {o().label}
                  </Text>
                </View>
              )}
            </Each>
            <If when={!matches().length}>
              <Text class={`text-xs px-2 py-1.5 ${C.fg3}`}>{p.options.length ? "No match. Try a code or a place name." : "The list did not load."}</Text>
            </If>
          </View>
        </Overlay>
      </If>
    </View>
  );
}

/** The centered auth card (auth.js `screen`): icon, title and line, then the step. */
function Card(p: { icon: string; title: string; line: string; lineDanger?: boolean; wide?: boolean; children?: JSX.Element }) {
  return (
    <View class={`rounded-2xl p-8 gap-4 ${C.pane} ${C.line}`} style={{ width: "100%", maxWidth: p.wide ? 720 : 420 }}>
      <View class="items-center gap-2">
        <View class={`rounded-full items-center justify-center ${C.inset}`} style={{ width: 44, height: 44 }}>
          <Text class={`text-lg ${C.fg2}`}>{p.icon}</Text>
        </View>
        <Text class={`text-xl font-semibold text-center ${C.fg}`}>{p.title}</Text>
        <Text class={`text-sm text-center ${p.lineDanger ? C.danger : C.fg2}`}>{p.line}</Text>
      </View>
      {p.children}
    </View>
  );
}

function Choice(p: { icon: string; title: string; line: string; onPress: () => void }) {
  return (
    <View onPress={p.onPress} activeStyle={{ opacity: 0.7 }} class={`flex-row items-start gap-3 p-4 rounded-xl ${C.line}`} accessibilityLabel={p.title}>
      <Text class={`text-lg ${C.accent}`}>{p.icon}</Text>
      <View class="flex-1 gap-1">
        <Text class={`text-sm font-semibold ${C.fg}`}>{p.title}</Text>
        <Text class={`text-xs ${C.fg2}`}>{p.line}</Text>
      </View>
    </View>
  );
}

// ============================================================ the app

type Step = "checking" | "account" | "waiting" | "setup" | "helpdesk-pin" | "helpdesk-key" | "taxpayer" | "done";

export default function R() {
  const step = signal<Step>("checking");
  const error = signal<string | null>(null);
  const busy = signal(false);
  const status = signal<CloudAccountStatus | null>(null);
  const started = signal<CloudSignInStart | null>(null);
  const auth = signal<AuthState | null>(null);
  /** First launch chose "Just me": Create also makes the new taxpayer the owner. */
  const pendingSolo = signal(false);
  const pin = signal("");
  const recoveryKey = signal("");
  const created = signal<Taxpayer | null>(null);
  let alive = true;
  cleanup(() => {
    alive = false;
  });

  // ---------------------------------------------------------------- account (gate.js / main.js finishUnlock)

  /** Approved: open the database, then go to setup (or the first taxpayer). */
  const unlock = async () => {
    await api.sessionOpened();
    await toSetup();
  };

  const toSetup = async () => {
    const a = await api.authState();
    auth.set(a);
    if (!a.mode) {
      step.set("setup");
    } else {
      pendingSolo.set(false);
      openNewTaxpayer();
    }
  };

  const check = async () => {
    step.set("checking");
    error.set(null);
    try {
      const s = await api.cloudStatus();
      status.set(s);
      if (s.signed_in) await unlock();
      else step.set("account");
    } catch (e) {
      error.set(errorText(e));
      step.set("account");
    }
  };
  check();

  const startAccountFlow = async (kind: "sign-in" | "create") => {
    if (busy()) return;
    busy.set(true);
    error.set(null);
    try {
      const s = kind === "create" ? await api.beginSignUp() : await api.beginSignIn();
      if (!alive) return;
      started.set(s);
      step.set("waiting");
      const st = await api.finishSignIn();
      if (!alive) return;
      status.set(st);
      await unlock();
    } catch (e) {
      error.set(errorText(e));
      step.set("account");
    } finally {
      busy.set(false);
    }
  };

  // ---------------------------------------------------------------- setup (auth.js renderSetup, main.js)

  const chooseSolo = async () => {
    error.set(null);
    try {
      const first = (await api.listTaxpayers())[0];
      if (first) {
        // A taxpayer exists already: it becomes the owner.
        await api.setupSolo(first.tin_root);
        created.set(first);
        step.set("done");
        return;
      }
    } catch (e) {
      return error.set(errorText(e));
    }
    // No taxpayer yet: type it first; Create finishes the setup.
    pendingSolo.set(true);
    openNewTaxpayer();
  };
  const chooseHelpdesk = () => {
    pin.set("");
    error.set(null);
    step.set("helpdesk-pin");
  };
  const submitHelpdeskPin = async () => {
    if (pin().length !== 4) return error.set("Type all 4 digits of the admin PIN.");
    busy.set(true);
    try {
      recoveryKey.set((await api.setupHelpdesk(pin())).recovery_key);
      pin.set("");
      error.set(null);
      step.set("helpdesk-key");
    } catch (e) {
      error.set(errorText(e));
    } finally {
      busy.set(false);
    }
  };
  const finishHelpdeskSetup = () => {
    recoveryKey.set("");
    pendingSolo.set(false);
    openNewTaxpayer();
  };
  const cancelSetup = () => {
    pendingSolo.set(false);
    error.set(null);
    step.set("setup");
  };

  // ---------------------------------------------------------------- new taxpayer (taxpayer.js openNewTaxpayer / createTaxpayer)

  const nTin = signal("");
  const nName = signal("");
  const nType = signal<TaxpayerType>("Individual");
  const nAddress = signal("");
  const nZip = signal("");
  const nRdo = signal("");
  const nPhone = signal("");
  const nEmail = signal("");
  const touched = signal<Set<string>>(new Set());
  const submitted = signal(false);
  const hostErrors = signal<Record<string, string>>({});
  const formError = signal<string | null>(null);
  const taken = signal<Taxpayer[]>([]);

  const rdos = resource(
    () => step() === "taxpayer",
    async () => (await api.listRdos()).map((r) => ({ value: r.code, label: `${r.code} - ${r.description}` })),
    { initialValue: [] as Option[] },
  );
  const zips = resource(
    () => step() === "taxpayer",
    async () => (await api.listZipCodes()).map((z) => ({ value: z.split(" - ")[0].trim(), label: z })),
    { initialValue: [] as Option[] },
  );

  const openNewTaxpayer = () => {
    for (const s of [nTin, nName, nAddress, nZip, nRdo, nPhone, nEmail]) s.set("");
    nType.set("Individual");
    touched.set(new Set());
    submitted.set(false);
    hostErrors.set({});
    formError.set(null);
    error.set(null);
    api.listTaxpayers().then(
      (list) => taken.set(list),
      () => taken.set([]),
    );
    step.set("taxpayer");
  };
  const touch = (k: string) => touched.update((t) => new Set(t).add(k));
  const edited = (k: string) => {
    const h = { ...hostErrors() };
    delete h[k];
    hostErrors.set(h);
    formError.set(null);
  };

  /** Every problem with the new taxpayer as typed, by field. */
  const problems = computed(() => {
    const e: Record<string, string> = {};
    const digits = nTin();
    if (digits.length !== 9) e.tin = "TIN is 9 digits, like 123-456-789.";
    else {
      const t = taken().find((x) => x.tin_root === digits);
      if (t) e.tin = `TIN ${formatTin(digits)} is already ${t.name}.`;
    }
    if (!nName().trim()) e.name = "Type the registered name.";
    const zip = nZip().trim();
    if (zip && !/^\d{4}$/.test(zip)) e.zip_code = "ZIP code is 4 digits, like 1226. Type a code or a place to search.";
    const rdo = nRdo().trim();
    if (rdo && rdos().length && !rdos().some((o) => o.value === rdo)) e.rdo_code = "Pick an RDO from the list. Type its code or district to search.";
    const phone = nPhone().trim();
    const phoneDigits = phone.replace(/\D/g, "").length;
    if (phone && (phoneDigits < 7 || phoneDigits > 13)) e.phone = "Type a phone number like 09171234567 or 028123 4567.";
    const email = nEmail().trim();
    if (email && !EMAIL_RE.test(email)) e.email = "Type an email like name@example.com.";
    return e;
  });
  /** The errors to show now: touched fields, all after Create, and the host's. */
  const shown = computed(() => {
    const out: Record<string, string> = { ...hostErrors() };
    for (const [k, v] of Object.entries(problems())) {
      const duplicate = k === "tin" && nTin().length === 9;
      if (submitted() || touched().has(k) || duplicate) out[k] = out[k] || v;
    }
    return out;
  });

  const createTaxpayer = async () => {
    submitted.set(true);
    formError.set(null);
    if (Object.keys(problems()).length || busy()) return;
    const digits = nTin();
    busy.set(true);
    try {
      const t = await api.createTaxpayer({
        tin_root: digits,
        name: nName().trim(),
        taxpayer_type: nType(),
        head_office: {
          registered_address: nAddress().trim(),
          zip_code: nZip().trim(),
          rdo_code: nRdo().trim(),
          phone: nPhone().trim(),
          email: nEmail().trim(),
        },
      });
      // First launch chose "Just me": this taxpayer is the owner, the admin.
      if (pendingSolo()) {
        try {
          await api.setupSolo(digits);
          pendingSolo.set(false);
        } catch (e) {
          formError.set(`${t.name} was added, but this computer was not set up for it: ${errorText(e)}`);
          created.set(t);
          return;
        }
      }
      created.set(t);
      step.set("done");
    } catch (e) {
      const text = errorText(e);
      if (/\bTIN\b/i.test(text)) hostErrors.set({ ...hostErrors(), tin: text });
      else formError.set(text);
    } finally {
      busy.set(false);
    }
  };

  /** Opens the new taxpayer's profile in the BIR Tax Profile app. */
  const openProfile = async () => {
    const t = created();
    if (!t?.ticket) return formError.set("BIR gave no ticket for this taxpayer; open them from the chat.");
    try {
      await rox.open({ app: "tax-profile", ticket: t.ticket, props: { ticket: t.ticket } });
    } catch (e) {
      formError.set(errorText(e));
    }
  };

  // ---------------------------------------------------------------- views

  const accountView = () => (
    <Card
      icon="☁"
      title="Welcome to eBIRForms"
      line={error() || status()?.detail || "Sign in with your Buwiz account to file from this computer."}
      lineDanger={!!error()}
    >
      <Button label={busy() ? "Waiting…" : "Sign in"} variant="primary" block disabled={busy()} onPress={() => startAccountFlow("sign-in")} />
      <Button label={busy() ? "Waiting…" : "Create account"} block disabled={busy()} onPress={() => startAccountFlow("create")} />
      <If when={error()}>
        <View class="items-center">
          <Button label="Check again" variant="ghost" onPress={check} />
        </View>
      </If>
    </Card>
  );

  const waitingView = () => (
    <Card
      icon="⧗"
      title="Approve this computer"
      line={
        started()?.browser_opened
          ? "Finish in the browser that opened, then return here. Check that it shows this code."
          : "Open the link below in a browser, sign in, and approve this computer with this code."
      }
    >
      <View class={`items-center rounded-xl py-4 ${C.inset}`}>
        <Text class={`text-2xl font-semibold ${C.fg}`} selectable style={{ fontFamily: "Menlo", letterSpacing: 4 }}>
          {started()?.user_code || ""}
        </Text>
      </View>
      <View class="gap-1">
        <Text class={`text-xs ${C.fg2}`}>Verification link</Text>
        <Text class={`text-sm ${C.accent}`} selectable>
          {started()?.verification_uri_complete || started()?.verification_uri || ""}
        </Text>
      </View>
      <View class="flex-row flex-wrap items-center justify-between gap-2">
        <Text class={`text-xs ${C.fg2}`}>Waiting for approval…</Text>
        <Button
          label="Copy link"
          onPress={() => void rox.copy(started()?.verification_uri_complete || started()?.verification_uri || "")}
        />
      </View>
    </Card>
  );

  const setupView = () => (
    <Card icon="👥" title="Who will use eBIRForms on this computer?" line="This decides who the admin is. Nobody can lock the others out.">
      <Choice
        icon="👤"
        title="Just me or my business"
        line="You are the admin. Your own tax profile is the admin account, with all its branches."
        onPress={chooseSolo}
      />
      <Choice
        icon="👥"
        title="I help other taxpayers"
        line="An RDO help desk, an accountant or a bookkeeper. You get a separate admin account, and each taxpayer signs in as themselves."
        onPress={chooseHelpdesk}
      />
      <ErrorText text={error()} />
    </Card>
  );

  const pinView = () => (
    <Card
      icon="🔑"
      title="Choose the admin PIN"
      line="The help desk admin signs in with TIN 000-000-000 and this PIN. It also opens any taxpayer who forgot theirs."
    >
      <View class="gap-2">
        <Text class={`text-xs ${C.fg2}`}>Admin PIN</Text>
        <TextInput value={pin} digits={4} secure big placeholder="4 digits" error={!!error()} />
      </View>
      <ErrorText text={error()} />
      <View class="flex-row items-center justify-between">
        <Button label="Back" variant="ghost" onPress={cancelSetup} />
        <Button label={busy() ? "Saving…" : "Continue"} variant="primary" disabled={busy()} onPress={submitHelpdeskPin} />
      </View>
    </Card>
  );

  const keyView = () => (
    <Card
      icon="🔑"
      title="Write down your recovery key"
      line="If you forget the admin PIN, this key signs you in once. It is shown only now. Keep it away from this computer."
    >
      <View class={`items-center rounded-xl py-4 ${C.inset}`}>
        <Text class={`text-xl font-semibold ${C.fg}`} selectable style={{ fontFamily: "Menlo" }}>
          {recoveryKey()}
        </Text>
      </View>
      <Text class={`text-xs text-center ${C.fg2}`}>Lost both? Whoever can log in to this Mac can still reset the admin PIN.</Text>
      <View class="flex-row justify-end">
        <Button label="I wrote it down" variant="primary" onPress={finishHelpdeskSetup} />
      </View>
    </Card>
  );

  const taxpayerView = () => (
    <View class="gap-5" style={{ width: "100%", maxWidth: 720 }}>
      <View class="gap-1">
        <Text class={`text-xl font-semibold ${C.fg}`}>{pendingSolo() ? "Your tax profile" : "New taxpayer"}</Text>
        <Text class={`text-sm ${C.fg2}`}>
          The 9-digit TIN and the registered name. The head office's address is optional; branches inherit it unless you change it.
        </Text>
      </View>
      <View class={`rounded-2xl p-5 gap-4 ${C.pane} ${C.line}`}>
        <View class="flex-row flex-wrap gap-4">
          <Field label="TIN" error={shown().tin}>
            <TinBoxes
              value={nTin}
              error={!!shown().tin}
              onBlur={() => {
                if (nTin().length) touch("tin");
              }}
              onEdit={() => {
                if (nTin().length === 9) touch("tin");
                edited("tin");
              }}
            />
          </Field>
          <Field label="Taxpayer type">
            <Combo
              value={nType as unknown as Signal<string>}
              options={TAXPAYER_TYPES.map((x) => ({ value: x, label: x }))}
              placeholder="Choose a type"
              max={6}
            />
          </Field>
          <Field label="Registered name" full error={shown().name}>
            <TextInput value={nName} placeholder="As registered with BIR" error={!!shown().name} onBlur={() => touch("name")} onEdit={() => edited("name")} />
          </Field>
          <Field label="Head-office address (optional)" full error={shown().registered_address}>
            <TextInput value={nAddress} placeholder="Street, barangay, city" onBlur={() => touch("registered_address")} onEdit={() => edited("registered_address")} />
          </Field>
          <Field label="ZIP code" error={shown().zip_code}>
            <Combo
              value={nZip}
              options={zips()}
              placeholder="Type a code or place, like 1226 or Makati"
              error={!!shown().zip_code}
              onPick={() => {
                touch("zip_code");
                edited("zip_code");
              }}
            />
          </Field>
          <Field label="RDO" error={shown().rdo_code}>
            <Combo
              value={nRdo}
              options={rdos()}
              placeholder="Type a code or district, like 050 or Makati"
              error={!!shown().rdo_code}
              onPick={() => {
                touch("rdo_code");
                edited("rdo_code");
              }}
            />
          </Field>
          <Field label="Phone" error={shown().phone}>
            <TextInput value={nPhone} phone placeholder="09171234567" error={!!shown().phone} onBlur={() => touch("phone")} onEdit={() => edited("phone")} />
          </Field>
          <Field label="Email" error={shown().email}>
            <TextInput value={nEmail} placeholder="name@example.com" error={!!shown().email} onBlur={() => touch("email")} onEdit={() => edited("email")} />
          </Field>
        </View>
        <If when={rdos.error() || zips.error()}>
          <View class="flex-row flex-wrap items-center gap-3">
            <Text class={`text-xs flex-1 ${C.danger}`}>{`Could not load the RDO and ZIP lists: ${errorText(rdos.error() || zips.error())}`}</Text>
            <Button
              label="Retry"
              onPress={() => {
                rdos.refetch();
                zips.refetch();
              }}
            />
          </View>
        </If>
        <ErrorText text={formError()} />
        <View class="flex-row justify-end gap-2">
          <If when={!auth()?.mode || pendingSolo()}>
            <Button label="Cancel" variant="ghost" onPress={cancelSetup} />
          </If>
          <Button label={busy() ? "Creating…" : "Create taxpayer"} variant="primary" disabled={busy()} onPress={createTaxpayer} />
        </View>
      </View>
    </View>
  );

  const doneView = () => (
    <Card icon="✓" title={`${created()?.name || ""} is ready`} line={`TIN ${formatTin(created()?.tin_root || "")}. Next, open the tax profile to add a tax year and choose its forms.`}>
      <Button label="Open tax profile" variant="primary" block onPress={openProfile} />
      <ErrorText text={formError()} />
      <View class="items-center">
        <Button label="Add another taxpayer" variant="ghost" onPress={openNewTaxpayer} />
      </View>
    </Card>
  );

  return (
    <View class={`flex-1 ${C.card}`} style={{ minHeight: 520 }}>
      <Scroll class="flex-1">
        {/* Rox's nsh ignores contentContainerClass: the padding and centering live here */}
        <View class="w-full items-center px-4 py-8" style={{ minHeight: 520 }}>
        <If when={step() === "checking"}>
          <Text class={`text-sm ${C.fg2}`}>Checking your Buwiz account…</Text>
        </If>
        <If when={step() === "account"}>{accountView()}</If>
        <If when={step() === "waiting"}>{waitingView()}</If>
        <If when={step() === "setup"}>{setupView()}</If>
        <If when={step() === "helpdesk-pin"}>{pinView()}</If>
        <If when={step() === "helpdesk-key"}>{keyView()}</If>
        <If when={step() === "taxpayer"}>{taxpayerView()}</If>
        <If when={step() === "done"}>{doneView()}</If>
        </View>
      </Scroll>
    </View>
  );
}
