// Test identities on a LOCAL Silicon Accounts stack, for tests/e2e/accounts_e2e.py.
//
// It drives the stack's own sign-in pages and development mail through the Silicon Accounts testkit, so it needs a
// silicon-accounts checkout whose testkit dependencies are installed, and the stack file:
//
//   SILICON_ACCOUNTS_DIR=/path/to/silicon-accounts COMMIT_TEST_STACK=/path/to/test-stack.json \
//     "$SILICON_ACCOUNTS_DIR/testkit/node_modules/.bin/tsx" tests/e2e/mint.mts <command> [options]
//
//   carbon --email E                          a first-party Carbon session (creates the Carbon if new):
//                                             {uuid, id, kind, access_token, refresh_token}
//   silicon --token T --handle H              a Silicon si:H in the care of the Carbon whose first-party
//                                             access token is T: {uuid, id, kind, stk, custodian}
//   slt --silicon si:H --stk STK --app A      a single-use, 2-minute short-lived token for app A
//   app-signin --app A --email E --redirect URI [--scope S] [--exchange]
//                                             the hosted sign-in for app A: {code, code_verifier, redirect_uri}, or
//                                             with --exchange the app's tokens (needs A's secret in the stack file)
//   approve --token T --code CODE             approve a CLI's device sign-in as the Carbon whose first-party
//                                             access token is T
//
// Every command prints one JSON object. A Carbon costs one email code per `carbon` and per `app-signin` (two more
// the first time, to create it): the stack allows ten codes per address in ten minutes, so reuse first-party tokens.
// ACCOUNTS_API_URL and MOCK_MESSAGING_URL override the stack file's URLs. Never point this at a deployed stack.
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const root = process.env.SILICON_ACCOUNTS_DIR;
const stackFile = process.env.COMMIT_TEST_STACK;
if (!root || !stackFile) {
  console.error('mint: set SILICON_ACCOUNTS_DIR (a silicon-accounts checkout) and COMMIT_TEST_STACK (the stack file)');
  process.exit(2);
}
const kit = await import(pathToFileURL(join(root, 'testkit', 'lib', 'index.ts')).href);
const STACK = JSON.parse(readFileSync(stackFile, 'utf8')) as {
  accounts_api_url: string;
  mock_messaging_url: string;
  apps: Record<string, { app_secret?: string }>;
};
const accountsUrl = process.env.ACCOUNTS_API_URL ?? STACK.accounts_api_url;
if (!/^https?:\/\/(localhost|127\.0\.0\.1)(:\d+)?\/?$/.test(accountsUrl)) {
  console.error(`mint: ${accountsUrl} is not a local stack; this tool only talks to Silicon Accounts on this machine`);
  process.exit(2);
}
const accounts = new kit.AccountsClient(accountsUrl);
const messaging = new kit.MockMessagingClient(process.env.MOCK_MESSAGING_URL ?? STACK.mock_messaging_url);

function arg(name: string, required = true): string {
  const i = process.argv.indexOf(`--${name}`);
  const v = i >= 0 ? process.argv[i + 1] : undefined;
  if (required && !v) {
    console.error(`mint: --${name} is required`);
    process.exit(2);
  }
  return v ?? '';
}
const flag = (name: string) => process.argv.includes(`--${name}`);
const out = (v: unknown) => console.log(JSON.stringify(v));

function secretOf(app: string): string {
  const secret = STACK.apps[app]?.app_secret;
  if (!secret) {
    console.error(`mint: the stack file has no secret for app '${app}'`);
    process.exit(2);
  }
  return secret;
}

/** Signs an existing Carbon in with an email code, or creates it first (account site sign-up) when it is new. */
async function carbonSession(email: string) {
  try {
    const { tokens, session } = await accounts.cliLogin(messaging, { email });
    return { tokens, me: await session.me() };
  } catch {
    await kit.signUpCarbon({ accounts, messaging, email });
    const { tokens, session } = await accounts.cliLogin(messaging, { email });
    return { tokens, me: await session.me() };
  }
}

const command = process.argv[2];
if (command === 'carbon') {
  const { tokens, me } = await carbonSession(arg('email'));
  out({ uuid: me.uuid, id: me.id, kind: 'carbon', access_token: tokens.access_token, refresh_token: tokens.refresh_token });
} else if (command === 'silicon') {
  const session = accounts.withToken(arg('token'));
  const me = await session.me();
  const handle = arg('handle').replace(/^si:/, '');
  const created = await session.createSilicon({ id: `si:${handle}`, display_name: handle }, randomUUID());
  out({
    uuid: created.silicon.uuid,
    id: created.silicon.id,
    kind: 'silicon',
    stk: created.stk,
    custodian: { uuid: me.uuid, id: me.id },
  });
} else if (command === 'slt') {
  const tokens = await accounts.siliconLogin(arg('silicon'), arg('stk'), 'commit e2e');
  const res = await fetch(`${accounts.url}/v1/me/short-lived-tokens`, {
    method: 'POST',
    headers: { authorization: `Bearer ${tokens.access_token}`, 'content-type': 'application/json' },
    body: JSON.stringify({ app_id: arg('app') }),
  });
  const body = await res.json();
  if (!res.ok) throw new Error(`short-lived token refused: ${res.status} ${JSON.stringify(body)}`);
  out(body);
} else if (command === 'app-signin') {
  const app = arg('app');
  const email = arg('email');
  const scope = flag('scope') ? arg('scope') : undefined;
  const r = await kit.signInWithCode({ accounts, messaging, appId: app, email, redirectUri: arg('redirect'), ...(scope ? { scope } : {}) });
  if (!r.code) throw new Error(`the sign-in ended without a code: ${r.error}`);
  if (flag('exchange')) {
    out(await accounts.app(app, secretOf(app)).exchangeCode(r.code, r.redirectUri, r.codeVerifier));
  } else {
    out({ code: r.code, code_verifier: r.codeVerifier, state: r.state, redirect_uri: r.redirectUri });
  }
} else if (command === 'approve') {
  const code = arg('code').toUpperCase();
  const res = await fetch(`${accounts.url}/v1/device/${encodeURIComponent(code)}/approve`, {
    method: 'POST',
    headers: { authorization: `Bearer ${arg('token')}` },
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`device approval refused: ${res.status} ${text}`);
  out({ approved: code, status: res.status });
} else {
  console.error('usage: mint.mts carbon|silicon|slt|app-signin|approve (see the header of this file)');
  process.exit(2);
}
