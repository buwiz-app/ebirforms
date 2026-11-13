// Ported from Books at cd3d8cf. Run via tools/bot/host-test.sh only.
// The real Bot app-server runs in a child; IPC reaches its assistant surface
// without a model request or a new host route. Never print response payloads:
// they can contain host tokens, tickets, signatures or taxpayer details.
import { fork } from "node:child_process";
import { once } from "node:events";
import { closeSync, existsSync, openSync, readFileSync, statSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";

if (process.argv[2] === "--broker") {
  const WT = process.argv[3];
  await import(pathToFileURL(`${WT}/server/src/app-server.mjs`).href);
  const { plugins } = await import(pathToFileURL(`${WT}/server/src/v1/broker.mjs`).href);
  const { v1AgentCall, v1Tools } = await import(pathToFileURL(`${WT}/server/src/v1/agent.mjs`).href);
  process.on("message", async ({ id, action, tool, args }) => {
    try {
      let result;
      if (action === "surface") result = v1Tools(plugins).map((t) => t.name);
      else if (action === "agent") {
        const job = {};
        result = { ...await v1AgentCall(plugins, job, tool, args ?? {}), cards: job.v1Cards ?? [] };
      }
      else if (action === "direct-agent") {
        result = await plugins.callTool("app.buwiz.ebirforms", tool, {}, {
          meta: { "buwiz/caller": { origin: "agent", plugin: "app.buwiz.ebirforms", ts: Date.now() } },
        });
      } else throw new Error("unknown test action");
      process.send({ id, result });
    } catch {
      process.send({ id, error: true });
    }
  });
  const shutdown = async () => {
    await plugins.stopAll();
    process.exit(0);
  };
  process.on("SIGTERM", shutdown);
  process.on("disconnect", shutdown);
} else {
  await main().catch(() => {
    console.error("FAIL  host test aborted (details suppressed; rerun with KEEP_TMP=1 for the broker log)");
    process.exitCode = 1;
  });
}

async function main() {
  const [WT, RUN, BIN, PORT] = process.argv.slice(2);
  if (!WT || !RUN || !BIN || !PORT || process.env.BUWIZ_KEYSTORE !== `file:${RUN}/keys`
      || process.env.BUWIZ_TRUST_FILE !== `${RUN}/trust.json`
      || process.env.EBIRFORMS_DATA_DIR !== `${RUN}/ebirforms-data`) {
    throw new Error("use the isolated shell wrapper");
  }
  const { openApp, envelope } = await import(pathToFileURL(`${WT}/server/test/v1-helpers.mjs`).href);
  const { generate, pubOf, verify } = await import(pathToFileURL(`${WT}/server/src/v1/ed.mjs`).href);
  const { signedBytes } = await import(pathToFileURL(`${WT}/server/src/v1/jcs.mjs`).href);
  const BASE = `http://127.0.0.1:${PORT}`;
  const PLUGIN = "app.buwiz.ebirforms";
  const signed = JSON.parse(readFileSync(`${RUN}/signed.json`, "utf8"));
  const log = openSync(`${RUN}/broker.log`, "a", 0o600);
  const child = fork(fileURLToPath(import.meta.url), ["--broker", WT], {
    cwd: `${WT}/server`, stdio: ["ignore", log, log, "ipc"],
    env: {
      PATH: process.env.PATH, HOME: RUN, TMPDIR: RUN, PORT,
      GROK_DATA_DIR: `${RUN}/data`, BUWIZ_TRUST_FILE: `${RUN}/trust.json`,
      BUWIZ_KEYSTORE: `file:${RUN}/keys`, BUWIZ_NO_KEYCHAIN: "1", MOCK: "1", NODE_DISABLE_COMPILE_CACHE: "1",
      EBIRFORMS_DATA_DIR: `${RUN}/ebirforms-data`, EBIRFORMS_ACCOUNT_URL: "http://127.0.0.1:1",
    },
  });
  let id = 0;
  const pending = new Map();
  child.on("message", ({ id, result, error }) => {
    const p = pending.get(id);
    if (!p) return;
    pending.delete(id);
    clearTimeout(p.timer);
    if (error) p.reject(new Error("broker test action failed")); else p.resolve(result);
  });
  child.on("error", () => {});
  child.on("exit", () => {
    for (const p of pending.values()) {
      clearTimeout(p.timer);
      p.reject(new Error("broker exited"));
    }
    pending.clear();
  });
  const broker = (action, tool, args) => new Promise((resolve, reject) => {
    const request = ++id;
    const timer = setTimeout(() => { pending.delete(request); reject(new Error("broker test action timed out")); }, 15000);
    pending.set(request, { resolve, reject, timer });
    child.send({ id: request, action, tool, args });
  });
  const results = [];
  const check = (name, ok) => {
    results.push(!!ok);
    console.log(`${ok ? "PASS" : "FAIL"}  ${name}`);
  };
  const refused = (r, code) => r.status >= 400 && r.json.ok === false && r.json.code === code;
  const value = (r) => r.json.result?.structuredContent?.result;
  try {
    let ready = false;
    for (let i = 0; i < 150; i++) {
      if (child.exitCode !== null || child.signalCode !== null) throw new Error("broker exited before startup");
      if (existsSync(`${RUN}/data/host-token`)) {
        try { ready = (await fetch(`${BASE}/health`, { signal: AbortSignal.timeout(1000) })).ok; } catch {}
        if (ready) break;
      }
      await new Promise((r) => setTimeout(r, 200));
    }
    if (!ready) throw new Error("broker startup timed out");
    const token = readFileSync(`${RUN}/data/host-token`, "utf8");
    const post = async (path, body, auth = true) => {
      const response = await fetch(BASE + path, {
        method: "POST", signal: AbortSignal.timeout(15000),
        headers: { "content-type": "application/json", ...(auth ? { "x-buwiz-host": token } : {}) },
        body: JSON.stringify(body),
      });
      return { status: response.status, json: await response.json() };
    };
    const spec = { command: BIN, args: ["mcp"], env: {
      EBIRFORMS_MANIFEST: `${RUN}/signed.json`, EBIRFORMS_DATA_DIR: `${RUN}/ebirforms-data`,
      BUWIZ_NO_KEYCHAIN: "1", EBIRFORMS_ACCOUNT_URL: "http://127.0.0.1:1",
    } };
    const noAuth = await post("/v1/host/plugins/inspect", spec, false);
    check("host route without X-Buwiz-Host is 401", noAuth.status === 401);
    const inspect = await post("/v1/host/plugins/inspect", spec);
    check("inspect verifies the signed eBIRForms manifest under the test key", inspect.status === 200
      && inspect.json.ok === true && inspect.json.manifest?.id === PLUGIN
      && inspect.json.manifest?.protocol === "1.0.0"
      && inspect.json.manifest?.publisher?.key_id === "ebirforms-test-01"
      && /^[0-9a-f]{64}$/.test(inspect.json.manifest_sha256));
    const install = await post("/v1/host/plugins/install", { ...spec, manifest_sha256: inspect.json.manifest_sha256 });
    check("install the reviewed manifest", install.status === 200 && install.json.ok === true && install.json.id === PLUGIN);
    if (install.json.ok !== true) throw new Error("installation failed");
    const wrong = await post("/v1/host/plugins/install", { ...spec, manifest_sha256: "0".repeat(64) });
    check("install with an unreviewed hash is refused", refused(wrong, "manifest_invalid"));
    const resolved = await post("/v1/host/resolve", { plugin: PLUGIN, kind: "command", name: "taxform", value: "2551Q" });
    check("/taxform 2551Q resolves to Forms with an opaque taxpayer ticket", resolved.status === 200
      && resolved.json.result === "open" && resolved.json.app === "forms" && resolved.json.props?.form === "2551Q"
      && /^tkt_/.test(resolved.json.props?.ticket ?? "") && resolved.json.ticket === resolved.json.props.ticket);
    const formsCard = await broker("agent", "ebirforms_app_open", { app: "forms" });
    check("assistant Forms open produces a full app card", formsCard.is_error === false
      && formsCard.cards.some((c) => c.plugin === PLUGIN && c.app === "forms" && c.placement === "full"));
    const deadlinesCard = await broker("agent", "ebirforms_app_open", { app: "deadlines" });
    check("assistant Deadlines open produces an inline card", deadlinesCard.is_error === false
      && deadlinesCard.cards.some((c) => c.plugin === PLUGIN && c.app === "deadlines" && c.placement === "inline"));
    const sessions = { open: async (req) => {
      const response = await post("/v1/host/sessions/open", req);
      if (response.status !== 200 || response.json.ok !== true) throw new Error("session open failed");
      return response.json;
    } };
    const forms = await openApp(sessions, signed, "forms");
    const deadlines = await openApp(sessions, signed, "deadlines");
    const seed = readFileSync(`${RUN}/keys/${PLUGIN}.key`, "utf8");
    const pluginPub = pubOf(seed.trim());
    const certificate = (open) => !!open.res.cert && verify(pluginPub, signedBytes(open.res.cert), open.res.cert_sig)
      && open.res.cert.app === open.app && open.res.cert.open_id === open.open_id;
    check("Forms opens with a verified certificate", certificate(forms));
    check("Deadlines card opens with a verified certificate and inline placement", certificate(deadlines)
      && signed.apps.find((a) => a.id === "deadlines").placement.includes("inline"));
    const source = await post("/v1/host/apps/source", { plugin: PLUGIN, app: "forms" });
    check("Forms source is served with its signed fingerprint", source.status === 200 && source.json.ok === true
      && source.json.source?.length > 0 && source.json.fingerprint === signed.apps.find((a) => a.id === "forms").fingerprint);
    const cardSource = await post("/v1/host/apps/source", { plugin: PLUGIN, app: "deadlines" });
    check("Deadlines source is served with its signed fingerprint", cardSource.status === 200 && cardSource.json.ok === true
      && cardSource.json.source?.length > 0 && cardSource.json.fingerprint === signed.apps.find((a) => a.id === "deadlines").fingerprint);
    const formsTools = signed.apps.find((a) => a.id === "forms").permissions.tools;
    check("Forms is granted the stuck-send state and release tools", ["ebirforms_app_queued_send_state", "ebirforms_app_release_abandoned_claim"].every((t) => formsTools.includes(t)));
    const env1 = envelope(forms, "ebirforms_app_screen_forms_index", { args: [] });
    const call = await post("/v1/host/call", env1);
    check("signed Forms read returns the form catalogue", call.status === 200 && call.json.ok === true
      && Array.isArray(value(call)) && value(call).some((f) => f.c === "2551Q"));
    const replay = await post("/v1/host/call", env1);
    check("the same signed envelope is refused as a replay", refused(replay, "replayed"));
    const calendar = await post("/v1/host/call", envelope(deadlines, "ebirforms_calendar_get", {}));
    check("signed Deadlines calendar read succeeds", calendar.status === 200 && calendar.json.ok === true && calendar.json.result?.isError === false);
    // Status belongs to the assistant, not to either app's permissions.tools.
    // Do not alter the shipped manifest merely to allow a host-test call.
    const status = await post("/v1/host/call", envelope(forms, "ebirforms_status", {}));
    check("signed ebirforms_status is refused outside Forms permissions", refused(status, "permission_denied"));
    const agentStatus = await broker("agent", "ebirforms_status");
    const state = JSON.parse(agentStatus.content);
    check("assistant status answers from the scratch database with sending off", agentStatus.is_error === false
      && state.taxpayers === 1 && state.sending === false && state.signed_in === false);
    // New taxpayer: a signed Tax Profile write, checked by field, answered by
    // ticket; the root is synthetic, and the host's answers never carry it
    const profile = await openApp(sessions, signed, "tax-profile");
    check("Tax Profile opens with a verified certificate", certificate(profile));
    const NEW_ROOT = "000111222";
    const newTaxpayer = (root, rdo) => ({ args_json: JSON.stringify([{ tin_root: root, name: "Synthetic New Taxpayer", taxpayer_type: "Individual",
      head_office: { rdo_code: rdo, registered_address: "Synthetic address", zip_code: "1600", phone: "", email: "new@example.invalid" } }]) });
    const CREATE = "ebirforms_app_create_taxpayer";
    const badTin = await post("/v1/host/call", envelope(profile, CREATE, newTaxpayer("0001112", "043")));
    check("signed New taxpayer with a bad TIN is refused by field", refused(badTin, "tool_error") && /tin_root: /.test(badTin.json.message ?? ""));
    const noRdo = await post("/v1/host/call", envelope(profile, CREATE, newTaxpayer(NEW_ROOT, "")));
    check("signed New taxpayer without an RDO is refused by field", refused(noRdo, "tool_error") && /rdo_code: /.test(noRdo.json.message ?? ""));
    const made = await post("/v1/host/call", envelope(profile, CREATE, newTaxpayer(NEW_ROOT, "043")));
    const madeTicket = value(made)?.ticket ?? "";
    check("signed New taxpayer saves and answers a ticket, not the TIN", made.status === 200 && made.json.ok === true
      && /^tkt_/.test(madeTicket) && value(made)?.title === "Synthetic New Taxpayer" && !JSON.stringify(made.json).includes(NEW_ROOT));
    const again = await post("/v1/host/call", envelope(profile, CREATE, newTaxpayer(NEW_ROOT, "043")));
    check("the same TIN again is refused without naming it", refused(again, "tool_error")
      && /already saved/.test(again.json.message ?? "") && !JSON.stringify(again.json).includes(NEW_ROOT));
    const newTitle = await post("/v1/host/tickets/title", { plugin: PLUGIN, ticket: madeTicket });
    check("the new taxpayer's ticket resolves to its name", newTitle.status === 200 && newTitle.json.valid === true
      && newTitle.json.title === "Synthetic New Taxpayer");
    const formsCreate = await post("/v1/host/call", envelope(forms, CREATE, newTaxpayer("000555666", "043")));
    check("New taxpayer is outside Forms permissions", refused(formsCreate, "permission_denied"));
    const agentCreate = await broker("direct-agent", CREATE);
    check("New taxpayer refuses origin agent inside the plugin", agentCreate.isError === true
      && agentCreate.structuredContent?.code === "permission_denied");
    const notGranted = await post("/v1/host/call", envelope(deadlines, "ebirforms_resolve", {}));
    check("a tool outside Deadlines permissions is refused", refused(notGranted, "permission_denied"));
    const forged = await post("/v1/host/call", envelope(forms, "ebirforms_app_screen_forms_index", { args: [] }, {}, generate().seed));
    check("an envelope signed by another key is refused", refused(forged, "bad_signature"));
    const names = await broker("surface");
    const accountTools = ["status", "sign_in_start", "sign_in_poll", "sign_out"].map((s) => `ebirforms_account_${s}`);
    check("all four account tools are absent from the assistant surface", accountTools.every((t) => !names.includes(t)));
    check("New taxpayer is absent from the assistant surface", !names.includes("ebirforms_app_create_taxpayer"));
    for (const tool of accountTools) {
      const agent = await broker("agent", tool);
      check(`${tool} is refused by the assistant surface`, agent.is_error === true);
      const direct = await broker("direct-agent", tool);
      check(`${tool} also refuses origin agent inside the plugin`, direct.isError === true
        && direct.structuredContent?.code === "permission_denied");
    }
    check("no account credentials were created", !existsSync(`${RUN}/ebirforms-data/buwiz-account.json`));
    check("plugin key is in the scratch file keystore with mode 0600", (statSync(`${RUN}/keys/${PLUGIN}.key`).mode & 0o777) === 0o600);
    for (const open of [forms, deadlines, profile]) {
      const closed = await post("/v1/host/sessions/close", { open_id: open.open_id });
      check(`${open.app} session closes cleanly`, closed.status === 200 && closed.json.ok === true);
    }
    const afterClose = await post("/v1/host/call", envelope(forms, "ebirforms_app_screen_forms_index", { args: [] }));
    check("calls from a closed app session are refused", refused(afterClose, "session_invalid"));
    const failed = results.filter((ok) => !ok).length;
    console.log(`\n${results.length - failed} pass, ${failed} fail`);
    if (failed) process.exitCode = 1;
  } finally {
    if (child.exitCode === null && child.signalCode === null) {
      const exited = once(child, "exit");
      child.kill("SIGTERM");
      const timer = setTimeout(() => child.kill("SIGKILL"), 5000);
      await exited;
      clearTimeout(timer);
    }
    closeSync(log);
  }
}
