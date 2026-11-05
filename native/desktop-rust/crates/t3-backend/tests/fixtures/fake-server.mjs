// A stand-in for the T3 Code server, for supervisor tests. It reads the bootstrap envelope the
// way the real server does (one JSON line on the bootstrap descriptor), then serves readiness and
// the auth endpoints. FAKE_MODE picks a misbehavior.
import { createHmac, timingSafeEqual } from "node:crypto";
import { appendFileSync, existsSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { createInterface } from "node:readline";

const mode = process.env.FAKE_MODE ?? "ok";
const record = process.env.FAKE_RECORD;
const state = process.env.FAKE_STATE;

if (process.argv.at(-2) !== "--bootstrap-fd" || process.argv.at(-1) !== "0") {
  console.error(`unexpected arguments: ${process.argv.slice(2).join(" ")}`);
  process.exit(64);
}

if (mode === "owned") {
  console.error("A T3 Code server already owns this state directory.");
  process.exit(78);
}

if (mode === "crash-first" && state && !existsSync(state)) {
  writeFileSync(state, "crashed");
  console.error("crashing on purpose");
  process.exit(3);
}

const WINDOW_MS = 12 * 60 * 60 * 1000;
const tokenFor = (secret, window) =>
  createHmac("sha256", secret).update(`t3-desktop-bootstrap:${window}`).digest("hex");
const isValid = (secret, token) => {
  const current = Math.floor(Date.now() / WINDOW_MS);
  return [current - 1, current, current + 1].some((window) => {
    const expected = Buffer.from(tokenFor(secret, window));
    const presented = Buffer.from(token);
    return expected.length === presented.length && timingSafeEqual(expected, presented);
  });
};

const readBody = (request) =>
  new Promise((resolve) => {
    let body = "";
    request.on("data", (chunk) => (body += chunk));
    request.on("end", () => resolve(body));
  });

const json = (response, status, value) => {
  response.writeHead(status, { "content-type": "application/json" });
  response.end(JSON.stringify(value));
};

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
lines.once("line", (line) => {
  lines.close();
  const bootstrap = JSON.parse(line);
  if (record) appendFileSync(record, `${line}\n`);
  if (mode === "ignore-term") process.on("SIGTERM", () => console.log("ignoring SIGTERM"));
  if (mode === "exit-after-ready") setTimeout(() => process.exit(7), 300);

  const server = createServer(async (request, response) => {
    const url = new URL(request.url, "http://localhost");
    if (url.pathname === "/.well-known/t3/environment") {
      if (mode === "never-ready") return json(response, 503, {});
      return json(response, 200, {
        environmentId: "local",
        label: "Fake",
        orchestrationProtocolVersion: Number(process.env.FAKE_PROTOCOL ?? 2),
      });
    }
    if (url.pathname === "/oauth/token" && request.method === "POST") {
      const form = new URLSearchParams(await readBody(request));
      if (!isValid(bootstrap.desktopBootstrapSecret, form.get("subject_token") ?? "")) {
        return json(response, 401, {
          _tag: "EnvironmentAuthInvalidError",
          code: "auth_invalid",
          reason: "credential_invalid",
          traceId: "fake",
        });
      }
      return json(response, 200, {
        access_token: `bearer-for-${form.get("client_label")}`,
        issued_token_type: "urn:ietf:params:oauth:token-type:access_token",
        token_type: "Bearer",
        expires_in: 3600,
        scope: "orchestration:operate",
      });
    }
    if (url.pathname === "/api/auth/websocket-ticket/rpc" && request.method === "POST") {
      if (!request.headers.authorization?.startsWith("Bearer bearer-for-")) {
        return json(response, 401, { _tag: "EnvironmentAuthInvalidError", traceId: "fake" });
      }
      return json(response, 200, { ticket: "ticket-1", expiresAt: new Date().toISOString() });
    }
    json(response, 404, {});
  });
  server.listen(bootstrap.port, bootstrap.host, () => {
    console.log(`listening on ${bootstrap.host}:${bootstrap.port}`);
    console.error("stderr works too");
  });
});
