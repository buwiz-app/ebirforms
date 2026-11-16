// The eBIRForms plugin against a running Buwiz broker, over its loopback HTTP
// routes. Run via tools/bot/host-test.sh only: it starts the broker, makes the
// signed manifest and the test key, and passes their folder, the plugin
// binary, the ports and the broker's pid here. Only Node built-ins, fetch and
// the SDK's protocol modules (BUWIZ_SDK_DIR) are used.
// Never print response payloads: they can contain host tokens, tickets,
// signatures or taxpayer details.
import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { pathToFileURL } from "node:url";

async function main() {
  const [RUN, BIN, PORT, MODEL_PORT, BROKER_PID] = process.argv.slice(2);
  const SDK = process.env.BUWIZ_SDK_DIR;
  // everything the broker reads sits inside the scratch directory
  if (!RUN || !BIN || !PORT || !MODEL_PORT || !BROKER_PID || !SDK
      || process.env.BUWIZ_KEYSTORE !== `file:${RUN}/keys`
      || process.env.BUWIZ_TRUST_FILE !== `${RUN}/trust.json`
      || process.env.EBIRFORMS_DATA_DIR !== `${RUN}/ebirforms-data`
      || process.env.BUWIZ_NO_KEYCHAIN !== "1") {
    throw new Error("use the isolated shell wrapper");
  }
  // the SDK's own protocol modules: keys, signatures, canonical JSON, permission digests
  const sdkv1 = (name) => import(pathToFileURL(`${SDK}/packages/conformance/src/v1/${name}.mjs`).href);
  const { generate, pubOf, random128, sign, verify } = await sdkv1("ed");
  const { signedBytes } = await sdkv1("jcs");
  const { permsDigest } = await sdkv1("manifest");
  const BASE = `http://127.0.0.1:${PORT}`;
  const PLUGIN = "app.buwiz.ebirforms";
  const signed = JSON.parse(readFileSync(`${RUN}/signed.json`, "utf8"));
  const results = [];
  const check = (name, ok) => {
    results.push(!!ok);
    console.log(`${ok ? "PASS" : "FAIL"}  ${name}`);
  };
  const refused = (r, code) => r.status >= 400 && r.json.ok === false && r.json.code === code;
  const value = (r) => r.json.result?.structuredContent?.result;
  const alive = () => { try { process.kill(Number(BROKER_PID), 0); return true; } catch { return false; } };
  let ready = false;
  for (let i = 0; i < 150 && !ready; i++) {
    if (!alive()) {
      const tail = readFileSync(`${RUN}/broker.log`, "utf8").split("\n").filter(Boolean).slice(-5).map((l) => l.slice(0, 200));
      console.error(`the broker exited before it answered; its log ends:\n${tail.join("\n")}`);
      throw new Error("broker exited before startup");
    }
    if (existsSync(`${RUN}/data/host-token`)) {
      try { ready = (await fetch(`${BASE}/health`, { signal: AbortSignal.timeout(1000) })).ok; } catch {}
    }
    if (!ready) await new Promise((r) => setTimeout(r, 200));
  }
  if (!ready) throw new Error("broker startup timed out");
  const meta = await fetch(`${BASE}/meta`, { signal: AbortSignal.timeout(5000) }).then((r) => r.json()).catch(() => ({}));
  const digest = createHash("sha256").update(readFileSync(process.env.BUWIZ_BROKER_BIN)).digest("hex");
  console.log(`broker: ${meta.broker ?? "unknown"} ${meta.version ?? "unknown"}, binary sha256 ${digest}`);
  const token = readFileSync(`${RUN}/data/host-token`, "utf8").trim();
  const request = async (method, path, body, auth = true) => {
    const response = await fetch(BASE + path, {
      method, signal: AbortSignal.timeout(15000),
      headers: { "content-type": "application/json", ...(auth ? { "x-buwiz-host": token } : {}) },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    return { status: response.status, json: await response.json() };
  };
  const post = (path, body, auth = true) => request("POST", path, body, auth);
  // a session key and the app's permissions digest, as the app host opens it
  const openApp = async (app) => {
    const ks = generate();
    const a = signed.apps.find((x) => x.id === app);
    const req = { plugin: signed.id, app, open_id: random128(), ks_pub: ks.pub, perms_sha256: permsDigest(a?.permissions), not_after: Date.now() + 3600_000 };
    const response = await post("/v1/host/sessions/open", req);
    if (response.status !== 200 || response.json.ok !== true) throw new Error("session open failed");
    return { ...req, ks, res: response.json };
  };
  // actor is who the signed call acts for: "person" (the person's own act in the app) or "agent"
  // (the assistant pressing the app); the broker derives the call's origin from it
  const envelope = (open, tool, args = {}, over = {}, signer = open.ks.seed) => {
    const env = { v: 1, typ: "buwiz-call", plugin: open.plugin, app: open.app, open_id: open.open_id, tool, args, actor: "person", ts: Date.now(), nonce: random128(), ...over };
    return { envelope: env, sig: sign(signer, signedBytes(env)) };
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

  // The assistant's side runs through a real chat: the broker's tool loop talks
  // to a scripted model on loopback, and what the broker offers the model and
  // answers to its tool calls is read from the requests it sends.
  const model = await scriptedModel(MODEL_PORT);
  try {
    const allowed = await post("/v1/host/tools/set", { bot: "chats", key: `v1:${PLUGIN}`, state: "on" });
    if (allowed.status !== 200 || allowed.json.ok !== true) throw new Error("could not switch the plugin on for chats");
    // one chat: the model's tool calls in one round; the round after carries the broker's answers
    const chat = async (name, calls) => {
      model.script(name, calls.map(([tool, input]) => ({ type: "tool_use", name: tool, input })));
      const thread = (await post("/thread/new", {})).json.id;
      const started = await post("/chat", { prompt: `[[script:${name}]] go`, thread });
      if (!Number.isInteger(started.json.id)) throw new Error(`chat ${name} gave no job id`);
      let job = {};
      for (let i = 0; i < 150 && !job.done; i++) {
        job = (await request("GET", `/status?id=${started.json.id}`)).json;
        if (!job.done) await new Promise((r) => setTimeout(r, 100));
      }
      const rounds = model.requests.filter((r) => r.script === name).map((r) => r.body);
      const answers = (rounds.find((b) => hasToolResults(b))?.messages?.at(-1)?.content ?? []).filter((b) => b.type === "tool_result");
      const turns = (await request("GET", `/turns?thread=${thread}`)).json.turns ?? [];
      if (job.done === true && !turns.some((t) => t.role === "ai")) throw new Error(`chat ${name} is done but its thread holds no reply`);
      const cards = turns.flatMap((t) => t.links ?? []).map((l) => l.card).filter(Boolean);
      const text = (a) => typeof a.content === "string" ? a.content : (a.content ?? []).map((c) => c.text ?? "").join("\n");
      return { done: job.done === true, tools: (rounds.find((b) => !hasToolResults(b))?.tools ?? []).map((t) => t.name), answers: answers.map((a) => ({ error: a.is_error === true, text: text(a) })), cards };
    };
    const formsChat = await chat("open-forms", [["ebirforms_app_open", { app: "forms" }]]);
    check("assistant Forms open produces a full app card", formsChat.done && formsChat.answers[0]?.error === false
      && formsChat.cards.some((c) => c.plugin === PLUGIN && c.app === "forms" && c.placement === "full"));
    const deadlinesChat = await chat("open-deadlines", [["ebirforms_app_open", { app: "deadlines" }]]);
    check("assistant Deadlines open produces an inline card", deadlinesChat.done && deadlinesChat.answers[0]?.error === false
      && deadlinesChat.cards.some((c) => c.plugin === PLUGIN && c.app === "deadlines" && c.placement === "inline"));
    const accountTools = ["status", "sign_in_start", "sign_in_poll", "sign_out", "tins", "tin_link"].map((s) => `ebirforms_account_${s}`);
    const CREATE = "ebirforms_app_create_taxpayer";
    const statusChat = await chat("status", [["ebirforms_status", {}]]);
    let state = {};
    try { state = JSON.parse(statusChat.answers[0]?.text); } catch {}
    check("assistant status answers from the scratch database with sending off", statusChat.answers[0]?.error === false
      && state.taxpayers === 1 && state.sending === false && state.signed_in === false);
    const refusedChat = await chat("refused", [...accountTools, CREATE].map((t) => [t, {}]));
    check("all six account tools are absent from the assistant surface", refusedChat.tools.length > 0
      && accountTools.every((t) => !refusedChat.tools.includes(t)));
    check("New taxpayer is absent from the assistant surface", !refusedChat.tools.includes(CREATE));
    accountTools.forEach((tool, i) => check(`${tool} is refused by the assistant surface`, refusedChat.answers[i]?.error === true));
    check("New taxpayer is refused by the assistant surface", refusedChat.answers[accountTools.length]?.error === true);
  } finally {
    await model.close();
  }

  const forms = await openApp("forms");
  const deadlines = await openApp("deadlines");
  // the plugin's public key, derived from its key file in the scratch keystore, must be the one the broker reports
  const pluginSeed = readFileSync(`${RUN}/keys/${PLUGIN}.key`, "utf8").trim();
  check("the broker reports the public key of the plugin's stored key", pubOf(pluginSeed) === install.json.plugin_pub);
  const certificate = (open) => !!open.res.cert && verify(pubOf(pluginSeed), signedBytes(open.res.cert), open.res.cert_sig)
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
  const otherTools = signed.apps.filter((a) => a.id !== "forms").flatMap((a) => a.permissions.tools || []);
  check("Forms is granted a person's Retry and no other app is", formsTools.includes("ebirforms_app_retry_queued") && !otherTools.includes("ebirforms_app_retry_queued"));
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
  // New taxpayer: a signed Tax Profile write, checked by field, answered by
  // ticket; the root is synthetic, and the host's answers never carry it
  const profile = await openApp("tax-profile");
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
  const agentCreate = await post("/v1/host/call", envelope(profile, CREATE, newTaxpayer("000777888", "043"), { actor: "agent" }));
  check("a signed New taxpayer call acting as the agent is refused as a person's to run", refused(agentCreate, "permission_denied")
    && /person's to run/.test(agentCreate.json.message ?? ""));
  const notGranted = await post("/v1/host/call", envelope(deadlines, "ebirforms_resolve", {}));
  check("a tool outside Deadlines permissions is refused", refused(notGranted, "permission_denied"));
  const forged = await post("/v1/host/call", envelope(forms, "ebirforms_app_screen_forms_index", { args: [] }, {}, generate().seed));
  check("an envelope signed by another key is refused", refused(forged, "bad_signature"));
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
}

// a scripted stand-in for the model on loopback: Messages over SSE. A script
// is the tool calls it makes when asked; a request that carries tool results gets the closing answer.
// The newest user text with [[script:<name>]] picks the script.
// whether the request's last message carries tool results
const hasToolResults = (body) => (body.messages?.at(-1)?.content ?? []).some?.((b) => b.type === "tool_result") === true;

async function scriptedModel(port) {
  const scripts = new Map();
  const requests = [];
  const server = createServer((req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      let body = {};
      try { body = JSON.parse(Buffer.concat(chunks).toString("utf8")); } catch {}
      if (!/\/v1\/messages$/.test(req.url)) { res.writeHead(404).end("{}"); return; }
      const texts = (body.messages ?? []).filter((m) => m.role === "user").map((m) => typeof m.content === "string" ? m.content : (m.content ?? []).filter((b) => b.type === "text").map((b) => b.text).join("\n"));
      const name = texts.reverse().map((t) => /\[\[script:([\w.-]+)\]\]/.exec(t)?.[1]).find(Boolean);
      const calls = scripts.get(name);
      requests.push({ script: name, body });
      if (!calls) { res.writeHead(200, { "content-type": "application/json" }); res.end(JSON.stringify({ id: "msg_x", type: "message", role: "assistant", model: body.model, content: [{ type: "text", text: "ok" }], stop_reason: "end_turn", stop_sequence: null, usage: { input_tokens: 1, output_tokens: 1 } })); return; }
      const content = hasToolResults(body) ? [{ type: "text", text: "Done." }] : calls.map((b, i) => ({ ...b, id: `toolu_${i}` }));
      const stop_reason = content.some((b) => b.type === "tool_use") ? "tool_use" : "end_turn";
      const message = { id: "msg_x", type: "message", role: "assistant", model: body.model, content, stop_reason, stop_sequence: null, usage: { input_tokens: 1, output_tokens: 1 } };
      if (!body.stream) { res.writeHead(200, { "content-type": "application/json" }); res.end(JSON.stringify(message)); return; }
      res.writeHead(200, { "content-type": "text/event-stream" });
      const ev = (type, data) => res.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`);
      ev("message_start", { message: { ...message, content: [], stop_reason: null } });
      content.forEach((b, index) => {
        if (b.type === "text") {
          ev("content_block_start", { index, content_block: { type: "text", text: "" } });
          ev("content_block_delta", { index, delta: { type: "text_delta", text: b.text } });
        } else {
          ev("content_block_start", { index, content_block: { type: "tool_use", id: b.id, name: b.name, input: {} } });
          ev("content_block_delta", { index, delta: { type: "input_json_delta", partial_json: JSON.stringify(b.input ?? {}) } });
        }
        ev("content_block_stop", { index });
      });
      ev("message_delta", { delta: { stop_reason, stop_sequence: null }, usage: { output_tokens: 1 } });
      ev("message_stop", {});
      res.end();
    });
  });
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(Number(port), "127.0.0.1", resolve); });
  return {
    requests,
    script: (name, calls) => scripts.set(name, calls),
    close: () => new Promise((r) => { server.closeAllConnections?.(); server.close(r); }),
  };
}

await main().catch(() => {
  console.error("FAIL  host test aborted (details suppressed; rerun with KEEP_TMP=1 for the broker log)");
  process.exitCode = 1;
});
