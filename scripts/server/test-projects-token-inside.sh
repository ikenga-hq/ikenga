#!/bin/bash
# provision.sh: one deploy token per project (PROJECTS_TOKEN_SECRETS), with
# PROJECTS_TOKEN_SECRET as the default. Run as root inside an ubuntu:24.04
# container (test-projects-token-container.sh) with real git over a loopback
# HTTP server that insists on a DIFFERENT fake token per repo, and records which
# token (if any) every request carried. That is how the test sees, from the far
# side, that a project is only ever given its own token.
#
#   org1, org2   need ORG token        (default PROJECTS_TOKEN_SECRET)
#   personal     needs PERSONAL token  (PROJECTS_TOKEN_SECRETS personal=...)
#   locked       needs PERSONAL token, but is listed with no token of its own
#                in scenario B: a stale/leaked personal token would let it in
set -euo pipefail

T="$(mktemp -d)"
PROV=/root/prov
PROVISION=/work/provision.sh
PROFILE="$PROV/profile.env"
SECRETS="$PROV/secrets"
OUT="$T/out"
PROJ=/srv/ikenga/projects
REQLOG="$T/requests.log"
ORG_TOKEN=FAKE-org-token-111
PERSONAL_TOKEN=FAKE-personal-token-222

fail() { echo "FAIL: $*" >&2; [[ -f "$OUT" ]] && { echo "--- last output ---" >&2; cat "$OUT" >&2; }; exit 1; }
pass() { echo "==> [Container] $* PASSED"; }
SERVER_PID=""
trap 'kill "$SERVER_PID" 2>/dev/null || true; rm -rf "$T"' EXIT

# ------------------------------------------------------------- fixtures

git config --global user.name test; git config --global user.email t@example.invalid
git config --global init.defaultBranch main
mkdir -p /fixtures/work /fixtures/http
git init -q -b main /fixtures/work
echo hello > /fixtures/work/README.md
git -C /fixtures/work add -A && git -C /fixtures/work commit -q -m first
for r in org1 org2 personal locked; do git clone -q --bare /fixtures/work "/fixtures/http/$r.git"; done
git clone -q --bare /fixtures/work /fixtures/local.git

# REPO=TOKEN pairs in argv-free env; every request is logged as "<repo> <who>"
# where who = the NAME of the token it carried (org|personal), none, or bad.
cat > "$T/githttp.py" <<'PY'
import base64, os, subprocess, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
ROOT, LOG = sys.argv[1], sys.argv[2]
NEED = {'org1': 'org', 'org2': 'org', 'personal': 'personal', 'locked': 'personal'}
TOK = {os.environ['TOK_ORG']: 'org', os.environ['TOK_PERSONAL']: 'personal'}
class H(BaseHTTPRequestHandler):
    def go(self):
        auth = self.headers.get('Authorization', '')
        who = 'none'
        if auth.startswith('Basic '):
            try:
                _, _, p = base64.b64decode(auth[6:]).decode().partition(':')
                who = TOK.get(p, 'bad')
            except Exception:
                who = 'bad'
        repo = self.path.split('/')[1].split('.')[0]
        with open(LOG, 'a') as f:
            f.write('%s %s\n' % (repo, who))
        n = int(self.headers.get('Content-Length') or 0)
        body = self.rfile.read(n) if n else b''
        if who != NEED.get(repo):
            self.send_response(401)
            self.send_header('WWW-Authenticate', 'Basic realm="git"')
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        path, _, qs = self.path.partition('?')
        env = dict(os.environ, GIT_PROJECT_ROOT=ROOT, GIT_HTTP_EXPORT_ALL='1', PATH_INFO=path,
                   QUERY_STRING=qs, REQUEST_METHOD=self.command, REMOTE_USER='x',
                   CONTENT_TYPE=self.headers.get('Content-Type', ''), CONTENT_LENGTH=str(len(body)))
        if self.headers.get('Content-Encoding'):
            env['HTTP_CONTENT_ENCODING'] = self.headers['Content-Encoding']
        r = subprocess.run(['git', 'http-backend'], input=body, env=env, capture_output=True)
        head, _, out = r.stdout.partition(b'\r\n\r\n')
        status, hdrs = 200, []
        for line in head.split(b'\r\n'):
            k, _, v = line.partition(b': ')
            if k.lower() == b'status':
                status = int(v.split()[0])
            elif k:
                hdrs.append((k.decode(), v.decode()))
        self.send_response(status)
        for k, v in hdrs:
            self.send_header(k, v)
        self.send_header('Content-Length', str(len(out)))
        self.end_headers()
        self.wfile.write(out)
    do_GET = go
    do_POST = go
    def log_message(self, *a):
        pass
HTTPServer(('127.0.0.1', 8099), H).serve_forever()
PY
: > "$REQLOG"
TOK_ORG=$ORG_TOKEN TOK_PERSONAL=$PERSONAL_TOKEN python3 "$T/githttp.py" /fixtures/http "$REQLOG" >/dev/null 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 50); do (exec 3<>/dev/tcp/127.0.0.1/8099) 2>/dev/null && break; sleep 0.1; done
git ls-remote http://127.0.0.1:8099/personal.git >/dev/null 2>&1 && fail "fixture: personal repo answered without a token"
git ls-remote http://x:$ORG_TOKEN@127.0.0.1:8099/personal.git >/dev/null 2>&1 && fail "fixture: personal repo accepted the org token"
git ls-remote http://x:$PERSONAL_TOKEN@127.0.0.1:8099/org1.git >/dev/null 2>&1 && fail "fixture: org repo accepted the personal token"
git ls-remote http://x:$ORG_TOKEN@127.0.0.1:8099/org1.git >/dev/null 2>&1 || fail "fixture: org repo refused the org token"
pass "fixture: every http repo accepts only its own token (negative controls)"

export IKENGA_PROJECTS_LOCK="$T/projects.lock"
install -d -m 0700 "$PROV"
URL=http://127.0.0.1:8099
RC=0
refr() { RC=0; "$PROVISION" refresh-projects --profile "$PROFILE" "$@" >"$OUT" 2>&1 || RC=$?; }
mkprofile() {   # args = profile lines
  printf '%s\n' "ACCOUNTS=(ada)" "SECRETS_FILE=$SECRETS" "$@" > "$PROFILE"; chmod 0600 "$PROFILE"
}
mksecrets() {   # args = secret lines
  printf '%s\n' "# fake values only" "$@" > "$SECRETS"; chmod 0600 "$SECRETS"
}
NORMAL_SECRETS=("[root] ORG_TOKEN=$ORG_TOKEN" "[root] PERSONAL_TOKEN=$PERSONAL_TOKEN")
mksecrets "${NORMAL_SECRETS[@]}"
resets() { rm -rf "$PROJ" /etc/gitconfig; : > "$REQLOG"; }

# ------------------------- A. default token + one project with its own token

mkprofile "PROJECTS=(\"org1=$URL/org1.git#main\" \"personal=$URL/personal.git#main\" \"org2=$URL/org2.git#main\" \"local=file:///fixtures/local.git\")" \
  "PROJECTS_TOKEN_SECRET=ORG_TOKEN" "PROJECTS_TOKEN_SECRETS=(\"personal=PERSONAL_TOKEN\")"

# Argv sampler (with a control, as in test-accounts-inside.sh).
sample_argv() { ( while :; do grep -alE 'FAKE-(org|personal)-toke[n]' /proc/[0-9]*/cmdline 2>/dev/null || true; done > "$1" ) & SAMPLER=$!; }
sample_argv "$T/control-hits"
bash -c 'sleep 1; true' FAKE-org-token-111 &
CONTROL=$!
sleep 0.6; kill "$SAMPLER" 2>/dev/null || true; wait "$SAMPLER" "$CONTROL" 2>/dev/null || true
[[ -s "$T/control-hits" ]] || fail "argv sampler control: a planted token was not seen"
sample_argv "$T/argv-hits"
resets
refr
kill "$SAMPLER" 2>/dev/null || true; wait "$SAMPLER" 2>/dev/null || true
[[ $RC -eq 0 ]] || fail "A: refresh exited $RC"
for p in org1 org2 personal local; do
  [[ "$(git --git-dir $PROJ/$p.git rev-parse refs/heads/main)" == "$(git -C /fixtures/work rev-parse HEAD)" ]] || fail "A: mirror $p did not fetch main"
done
[[ ! -s "$T/argv-hits" ]] || fail "A: a deploy token appeared in a process argv"
! grep -rqa 'FAKE-' $PROJ /etc/gitconfig 2>/dev/null || fail "A: a deploy token was written into a mirror or /etc/gitconfig"
pass "A1: org projects fetch with the default token, 'personal' with its own, a file:// project with none; no token in argv or the mirrors"

# What each repo saw: only ever its own token (a 401 probe with 'none' first is git's normal challenge).
for r in org1 org2; do
  grep -q "^$r org$" "$REQLOG" || fail "A2: $r was never served with the org token"
  ! grep "^$r " "$REQLOG" | grep -qv "^$r \(org\|none\)$" || fail "A2: $r saw a token that is not the org one: $(grep "^$r " "$REQLOG" | sort -u | tr '\n' ' ')"
done
grep -q '^personal personal$' "$REQLOG" || fail "A2: personal was never served with its own token"
! grep '^personal ' "$REQLOG" | grep -qv '^personal \(personal\|none\)$' || fail "A2: personal saw another token: $(grep '^personal ' "$REQLOG" | sort -u | tr '\n' ' ')"
! grep -q ' bad$' "$REQLOG" || fail "A2: a request carried an unknown token"
pass "A2: server-side, each repo saw only its own token (org1/org2 -> org, personal -> personal); no cross-project token"

refr
[[ $RC -eq 0 ]] || fail "A3: rerun exited $RC"
pass "A3: a second run (fetch into existing mirrors) succeeds the same way"

# The dry run (mirrors exist now) names the secret, never the value.
: > "$REQLOG"
refr --dry-run
[[ $RC -eq 0 ]] || fail "A4: dry run exited $RC"
grep -qF "[dry-run] fetch org1 from $URL/org1.git (deploy token: ORG_TOKEN)" "$OUT" || fail "A4: org1 does not name the default secret"
grep -qF "[dry-run] fetch org2 from $URL/org2.git (deploy token: ORG_TOKEN)" "$OUT" || fail "A4: org2 does not name the default secret"
grep -qF "[dry-run] fetch personal from $URL/personal.git (deploy token: PERSONAL_TOKEN)" "$OUT" || fail "A4: personal does not name its own secret"
grep -qF "[dry-run] fetch local from file:///fixtures/local.git" "$OUT" && ! grep -F "fetch local" "$OUT" | grep -q 'deploy token' || fail "A4: the file:// project names a token"
! grep -q 'FAKE-' "$OUT" || fail "A4: dry run printed a token value"
[[ ! -s "$REQLOG" ]] || fail "A4: dry run contacted the server"
pass "A4: dry run shows which secret each project resolves to (name only) and fetches nothing"

# ----------- B. no default; one project with a token, one with NONE after it

resets
mkprofile "PROJECTS=(\"personal=$URL/personal.git#main\" \"locked=$URL/locked.git#main\")" \
  "PROJECTS_TOKEN_SECRETS=(\"personal=PERSONAL_TOKEN\")"
refr
[[ $RC -ne 0 ]] || fail "B: the token-less private project did not fail the run"
grep -q 'project locked: cannot fetch' "$OUT" || fail "B: no failure for 'locked': $(cat "$OUT")"
[[ -d $PROJ/personal.git && ! -e $PROJ/locked.git ]] || fail "B: mirrors are wrong (personal should exist, locked should not)"
grep -q '^personal personal$' "$REQLOG" || fail "B: personal was not served"
grep -q '^locked ' "$REQLOG" || fail "B: locked was never contacted"
! grep '^locked ' "$REQLOG" | grep -qv '^locked none$' || fail "B: locked (no token) was sent a credential, the previous project's token leaked: $(grep '^locked ' "$REQLOG" | sort -u | tr '\n' ' ')"
! grep -q 'FAKE-' "$OUT" || fail "B: a token reached the run output"
pass "B: with no default token, a project that lists none is fetched with NO credential at all, even straight after a project that has one"

# Same, reversed order: the token-less project first.
resets
mkprofile "PROJECTS=(\"locked=$URL/locked.git#main\" \"personal=$URL/personal.git#main\")" \
  "PROJECTS_TOKEN_SECRETS=(\"personal=PERSONAL_TOKEN\")"
refr
[[ $RC -ne 0 ]] && [[ -d $PROJ/personal.git && ! -e $PROJ/locked.git ]] || fail "B2: order change broke it"
! grep '^locked ' "$REQLOG" | grep -qv '^locked none$' || fail "B2: locked was sent a credential"
pass "B2: order does not matter"

# ------------------------------------- C. the old single-token profile, as is

resets
mkprofile "PROJECTS=(\"org1=$URL/org1.git#main\" \"org2=$URL/org2.git#main\")" "PROJECTS_TOKEN_SECRET=ORG_TOKEN"
refr
[[ $RC -eq 0 ]] || fail "C: legacy single-token profile exited $RC"
[[ -d $PROJ/org1.git && -d $PROJ/org2.git ]] || fail "C: legacy profile did not mirror both"
refr --dry-run
[[ $RC -eq 0 ]] || fail "C: legacy dry run exited $RC"
grep -qxF "    [dry-run] fetch org1 from $URL/org1.git" "$OUT" || fail "C: legacy dry-run line changed"
! grep -q 'deploy token:' "$OUT" || fail "C: a profile without PROJECTS_TOKEN_SECRETS now names its secret in the dry run"
pass "C: a profile with only PROJECTS_TOKEN_SECRET behaves as before (dry-run lines unchanged)"

# ------------------------------------------------------------ D. validation

bad() {   # bad <expected message fragment> <profile lines...>
  local want="$1"; shift
  mkprofile "$@"
  refr --dry-run
  [[ $RC -ne 0 ]] || fail "D: expected failure for: $want"
  grep -q -- "$want" "$OUT" || fail "D: wanted '$want' in: $(cat "$OUT")"
  ! grep -q 'FAKE-' "$OUT" || fail "D: a token value reached the output"
}
P="PROJECTS=(\"personal=$URL/personal.git\" \"org1=$URL/org1.git\")"
bad "names 'persnal', which is not in PROJECTS" "$P" 'PROJECTS_TOKEN_SECRETS=("persnal=PERSONAL_TOKEN")'
bad "names 'personal org1', which is not in PROJECTS" "$P" 'PROJECTS_TOKEN_SECRETS=("personal org1=PERSONAL_TOKEN")'
bad "names 'personal' twice" "$P" 'PROJECTS_TOKEN_SECRETS=("personal=PERSONAL_TOKEN" "personal=ORG_TOKEN")'
bad "is not a usable secret name" "$P" 'PROJECTS_TOKEN_SECRETS=("personal=IKENGA_TOKEN")'
bad "is not a usable secret name" "$P" 'PROJECTS_TOKEN_SECRETS=("personal=has space")'
bad "must look like project=SECRET_NAME" "$P" 'PROJECTS_TOKEN_SECRETS=("personal")'
bad "must look like project=SECRET_NAME" "$P" 'PROJECTS_TOKEN_SECRETS=("personal=")'
bad "PROJECTS_TOKEN_SECRETS (personal) 'NO_SUCH_TOKEN' must appear exactly once in SECRETS_FILE (found 0)" "$P" 'PROJECTS_TOKEN_SECRETS=("personal=NO_SUCH_TOKEN")'
bad "PROJECTS_TOKEN_SECRET 'NO_SUCH_DEFAULT' must appear exactly once in SECRETS_FILE (found 0)" "$P" 'PROJECTS_TOKEN_SECRET=NO_SUCH_DEFAULT' 'PROJECTS_TOKEN_SECRETS=("personal=PERSONAL_TOKEN")'
mksecrets "${NORMAL_SECRETS[@]}" "[root] PERSONAL_TOKEN=$PERSONAL_TOKEN"
bad "PROJECTS_TOKEN_SECRETS (personal) 'PERSONAL_TOKEN' must appear exactly once in SECRETS_FILE (found 2)" "$P" 'PROJECTS_TOKEN_SECRETS=("personal=PERSONAL_TOKEN")'
mksecrets "${NORMAL_SECRETS[@]}"
printf '%s\n' "ACCOUNTS=(ada)" "$P" 'PROJECTS_TOKEN_SECRETS=("personal=PERSONAL_TOKEN")' > "$PROFILE"
refr --dry-run
[[ $RC -ne 0 ]] && grep -q 'PROJECTS_TOKEN_SECRETS needs SECRETS_FILE' "$OUT" || fail "D: no SECRETS_FILE was not refused"
pass "D: typo'd project, duplicate project, bad/reserved secret name, malformed entry, secret missing or duplicated in SECRETS_FILE, and no SECRETS_FILE all die with a message that names no value"

# A secret scoped to accounts still works but is warned about, like the default.
mksecrets "[root] ORG_TOKEN=$ORG_TOKEN" "[ada] PERSONAL_TOKEN=$PERSONAL_TOKEN"
mkprofile "$P" 'PROJECTS_TOKEN_SECRETS=("personal=PERSONAL_TOKEN")'
refr --dry-run
grep -q "WARNING: PERSONAL_TOKEN is the deploy token and is scoped 'ada'" "$OUT" || fail "E: no scope warning: $(cat "$OUT")"
! grep -q "WARNING: ORG_TOKEN" "$OUT" || fail "E: warned about a root-scoped secret"
pass "E: a per-project token scoped to an account (not root) gets the same warning as the default"
