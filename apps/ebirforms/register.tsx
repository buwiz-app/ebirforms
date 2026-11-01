// Buwiz Sign-in — this mini app's Buwiz account, as a Buwiz Bot mini app.
//
// The layout of the desktop's account gate (app/shell/gate.js, account.js):
// a centered card that says whether this computer is signed in, a Sign in
// button, then the code to approve at account.buwiz.com/device while the
// app waits, then who is signed in with Sign out. Sign-in is account.buwiz.com's
// device flow (RFC 8628), run by the mini app (`ebirforms_account_*`): the
// app never sees a token, only the code to type and who is signed in.
//
// Signing in turns filing on: a return queued in BIR Forms is then sent to
// the BIR through this account and comes back with a receipt. The desktop's first-run setup (solo or help desk, admin PIN) and the
// taxpayer-database unlock are left out: this mini app keeps its own data.

import { signal, computed, cleanup, Text, View, Scroll } from "nsh";
import type { JSX } from "nsh";
import { buwiz } from "buwiz";

// ============================================================ host calls
// Every call goes through `account`: the Bot's bridge to the mini app's
// sign-in tools, which answer `{signed_in, state?, email?, user_code?,
// verification_uri?, verification_uri_complete?, expires_in?, interval?}`.

interface Account {
  signed_in: boolean;
  state?: "pending" | "signed_in" | "signed_out" | "expired" | "denied";
  email?: string;
  user_code?: string;
  verification_uri?: string;
  verification_uri_complete?: string;
  expires_in?: number;
  interval?: number;
  note?: string;
}

async function account(tool: "status" | "sign_in_start" | "sign_in_poll" | "sign_out"): Promise<Account> {
  const r: any = await buwiz.call(`ebirforms_account_${tool}`, {});
  return (r?.result || { signed_in: false }) as Account;
}

/** account.buwiz.com, the only page this app opens (manifest `open.external`). */
const ACCOUNT_ORIGIN = "https://account.buwiz.com";

function errorText(e: unknown): string {
  return String(e && (e as any).message ? (e as any).message : e)
    .replace(/^Error:\s*/, "")
    .replace(/^ebirforms_account_[a-z_]+:\s*/, "");
}

// ============================================================ UI kit (Sand theme)

const C = {
  pane: "bg-white dark:bg-[#161616]",
  card: "bg-[#f5f5f5] dark:bg-[#1f1f1f]",
  inset: "bg-black/5 dark:bg-white/5",
  fg: "text-[#1c1c1c] dark:text-[#f5f5f5]",
  fg2: "text-[#1c1c1c99] dark:text-[#f5f5f599]",
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
      focusable={!p.disabled}
      accessibilityRole="button"
      accessibilityLabel={p.label}
      class={`px-4 py-2 rounded-md items-center justify-center border-2 border-transparent focus:border-[#1680f3] dark:focus:border-[#1884fc] ${box()} ${p.disabled ? "opacity-50" : ""} ${p.block ? "" : "self-start"}`}
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

/** The centered auth card (auth.js `screen`): icon, title and line, then the step. */
function Card(p: { icon: string; title: string; line: string; lineDanger?: boolean; children?: JSX.Element }) {
  return (
    <View class={`rounded-2xl p-8 gap-4 ${C.pane} ${C.line}`} style={{ width: "100%", maxWidth: 420 }}>
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

// ============================================================ the app

type Step = "checking" | "account" | "waiting" | "signed-in" | "expired" | "denied";

export default function R() {
  const step = signal<Step>("checking");
  const error = signal<string | null>(null);
  const busy = signal(false);
  const status = signal<Account | null>(null);
  const code = signal<Account | null>(null);

  // One poll timer at a time; a new attempt or leaving the app ends it.
  let attempt = 0;
  let timer: any = null;
  const stopPolling = () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
  };
  cleanup(() => {
    attempt++;
    stopPolling();
  });

  const show = (a: Account) => {
    status.set(a);
    if (a.signed_in) step.set("signed-in");
    else if (a.state === "pending") {
      code.set(a);
      step.set("waiting");
    } else step.set("account");
  };

  const check = async () => {
    step.set("checking");
    error.set(null);
    try {
      const a = await account("status");
      show(a);
      if (a.state === "pending") poll(++attempt, a.interval || 5);
    } catch (e) {
      error.set(errorText(e));
      step.set("account");
    }
  };
  check();

  /** Asks once, at the server's interval, until approved, declined or expired. */
  const poll = (mine: number, seconds: number) => {
    stopPolling();
    timer = setTimeout(async () => {
      if (mine !== attempt) return;
      try {
        const a = await account("sign_in_poll");
        if (mine !== attempt) return;
        if (a.state === "pending") {
          code.set(a);
          poll(mine, a.interval || seconds);
        } else if (a.state === "expired") step.set("expired");
        else if (a.state === "denied") step.set("denied");
        else show(a);
      } catch (e) {
        if (mine !== attempt) return;
        error.set(errorText(e));
        step.set("account");
      }
    }, seconds * 1000);
  };

  const signIn = async () => {
    if (busy()) return;
    const mine = ++attempt;
    busy.set(true);
    error.set(null);
    try {
      const a = await account("sign_in_start");
      if (mine !== attempt) return;
      show(a);
      if (a.state === "pending") poll(mine, a.interval || 5);
    } catch (e) {
      if (mine !== attempt) return;
      error.set(errorText(e));
      step.set("account");
    } finally {
      if (mine === attempt) busy.set(false);
    }
  };

  /** Back to the account page; the code waiting in the browser no longer counts here. */
  const cancel = () => {
    attempt++;
    stopPolling();
    busy.set(false);
    code.set(null);
    error.set(null);
    step.set("account");
  };

  const signOut = async () => {
    busy.set(true);
    try {
      const a = await account("sign_out");
      status.set(a);
      error.set(a.note || null);
      step.set("account");
    } catch (e) {
      error.set(errorText(e));
    } finally {
      busy.set(false);
    }
  };

  const signInLink = () => code()?.verification_uri_complete || code()?.verification_uri || "";
  /** The Bot opens account.buwiz.com only; any other server's page is shown to copy. */
  const canOpen = computed(() => signInLink().startsWith(`${ACCOUNT_ORIGIN}/`));
  const openLink = () => {
    buwiz.open({ url: signInLink() }).catch((e: unknown) => error.set(errorText(e)));
  };
  const minutes = () => Math.max(1, Math.round((code()?.expires_in || 0) / 60));

  // ---------------------------------------------------------------- views

  const accountView = () => (
    <Card icon="☁" title="Sign in to Buwiz" line="eBIRForms files returns with the BIR through your Buwiz account. Preparing and saving returns works without it.">
      <View class={`rounded-xl px-4 py-3 gap-1 ${C.inset}`}>
        <Text class={`text-sm ${C.fg}`}>This computer isn't signed in to Buwiz yet.</Text>
      </View>
      <Button label={busy() ? "Waiting…" : "Sign in"} variant="primary" block disabled={busy()} onPress={() => void signIn()} />
      <ErrorText text={error()} />
      <If when={error()}>
        <View class="items-center">
          <Button label="Check again" variant="ghost" onPress={() => void check()} />
        </View>
      </If>
    </Card>
  );

  const waitingView = () => (
    <Card icon="⧗" title="Approve this computer" line="Open the sign-in page, sign in to Buwiz, and check that it shows this code.">
      <View class={`items-center rounded-xl py-5 gap-2 ${C.inset}`}>
        <Text class={`text-3xl font-semibold ${C.fg}`} selectable style={{ fontFamily: "Menlo", letterSpacing: 6 }}>
          {code()?.user_code || ""}
        </Text>
        <Button label="Copy code" variant="ghost" onPress={() => void buwiz.copy(code()?.user_code || "")} />
      </View>
      <If when={canOpen()}>
        <Button label="Open account.buwiz.com/device" variant="primary" block onPress={openLink} />
      </If>
      <View class="gap-1">
        <Text class={`text-xs ${C.fg2}`}>Sign-in page</Text>
        <Text class={`text-sm ${C.accent}`} selectable>
          {signInLink()}
        </Text>
      </View>
      <ErrorText text={error()} />
      <View class="flex-row flex-wrap items-center justify-between gap-2">
        <Text class={`text-xs ${C.fg2}`}>{`Waiting for you to approve… (the code works for ${minutes()} min)`}</Text>
        <View class="flex-row gap-2">
          <Button label="Cancel" variant="ghost" onPress={cancel} />
          <Button label="Copy the sign-in link" onPress={() => void buwiz.copy(signInLink())} />
        </View>
      </View>
    </Card>
  );

  const signedInView = () => (
    <Card icon="✓" title="Signed in" line={`Signed in to Buwiz as ${status()?.email || "your account"}.`}>
      <View class={`rounded-xl px-4 py-3 gap-1 ${C.inset}`}>
        <Text class={`text-sm ${C.fg}`}>Returns you queue in BIR Forms are sent to the BIR through this account, and each comes back with a receipt.</Text>
      </View>
      <ErrorText text={error()} />
      <View class="items-center">
        <Button label={busy() ? "Signing out…" : "Sign out"} variant="ghost" disabled={busy()} onPress={() => void signOut()} />
      </View>
    </Card>
  );

  const endedView = (title: string, line: string) => (
    <Card icon="!" title={title} line={line} lineDanger>
      <Button label="Try again" variant="primary" block disabled={busy()} onPress={() => void signIn()} />
      <View class="items-center">
        <Button label="Cancel" variant="ghost" onPress={cancel} />
      </View>
    </Card>
  );

  return (
    <View class={`flex-1 ${C.card}`} style={{ minHeight: 520 }}>
      <Scroll class="flex-1">
        {/* the Bot's nsh ignores contentContainerClass: the padding and centering live here */}
        <View class="w-full items-center px-4 py-8" style={{ minHeight: 520 }}>
          <If when={step() === "checking"}>
            <Text class={`text-sm ${C.fg2}`}>Checking your Buwiz account…</Text>
          </If>
          <If when={step() === "account"}>{accountView()}</If>
          <If when={step() === "waiting"}>{waitingView()}</If>
          <If when={step() === "signed-in"}>{signedInView()}</If>
          <If when={step() === "expired"}>{endedView("The code expired", "Sign-in codes work for 15 minutes. Start again for a new one.")}</If>
          <If when={step() === "denied"}>{endedView("Sign-in declined", "The sign-in was declined on account.buwiz.com. Start again if that was a mistake.")}</If>
        </View>
      </Scroll>
    </View>
  );
}
