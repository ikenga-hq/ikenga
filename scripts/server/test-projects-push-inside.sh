#!/bin/bash
# provision.sh: PROJECTS_PUSH_TOKEN_SECRETS (accounts push to the upstream with a
# secret that is an environment variable of their sessions) and
# PROJECTS_SHARED_MAIN (a team repo everyone pushes straight to main). Run as root
# inside an ubuntu:24.04 container (test-projects-push-container.sh) with real git
# and real accounts, against a loopback smart-HTTP git server that ACCEPTS PUSHES
# and records, per request, which account's token it carried. That is how the
# test sees from the far side that the helper emitted the env token and git used it.
#
# Accounts: ada = a person (her clone is made BEFORE the project is shared-main),
# rex = an agent, eve = a person. Each has her own value of TEAM_TOK (a scoped
# secret), so the server log says WHICH account's token a request used.
# Projects: team (shared main, push token), other (plain: no helper, a different
# repo on the same host).
set -euo pipefail

T="$(mktemp -d)"
PROV=/root/prov
PROVISION=/work/provision.sh
PROFILE="$PROV/profile.env"
SECRETS="$PROV/secrets"
OUT="$T/out"
PROJ=/srv/ikenga/projects
PRINC=/var/lib/ikenga-test/principals
SD="$T/systemd"
INSTALL="$T/opt-ikenga"
REQLOG="$T/requests.log"
URL=http://127.0.0.1:8099
TEAM="$URL/team.git"
ROOT_TOK=FAKE-root-token-000
ADA_TOK=FAKE-ada-token-111
REX_TOK=FAKE-rex-token-222
EVE_TOK=FAKE-eve-token-333
ALL_TOKS=("$ROOT_TOK" "$ADA_TOK" "$REX_TOK" "$EVE_TOK")

fail() { echo "FAIL: $*" >&2; [[ -f "$OUT" ]] && { echo "--- last output ---" >&2; cat "$OUT" >&2; }; exit 1; }
pass() { echo "==> [Container] $* PASSED"; }
SERVER_PID=""
trap 'kill "$SERVER_PID" 2>/dev/null || true; rm -rf "$T"' EXIT

# ------------------------------------------------------------------ host

mkacct() {   # name uid : mimics what `ikenga-server accounts create` leaves behind
  groupadd -g "$2" "ik-$1"
  useradd -u "$2" -g "$2" -M -d "$PRINC/$2/home" -s /bin/sh "ik-$1"
  install -d -m 0700 -o "$2" -g "$2" "$PRINC/$2/home"
}
mkdir -p "$PRINC" "$SD" "$T/bin" "$T/sbin" "$INSTALL"
mkacct ada 20001; mkacct rex 20003; mkacct eve 20005
home_of() { echo "$PRINC/$(id -u "ik-$1")/home"; }
wt_of() { echo "$(home_of "$1")/projects/$2"; }
printf '#!/bin/bash\nexit 0\n' > "$T/bin/systemctl"; chmod +x "$T/bin/systemctl"

# as <account> [VAR=value...] cmd... : the daemon's way of running things for an account
# (own uid, no supplementary groups, empty environment) plus whatever env the caller adds.
as() {
  local n="$1" u; shift
  u="$(id -u "ik-$n")"
  ( cd / && setpriv --reuid="$u" --regid="$u" --clear-groups \
      env -i HOME="$PRINC/$u/home" PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 GIT_TERMINAL_PROMPT=0 \
      GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.invalid GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.invalid "$@" )
}
cfg() { as "$1" git -C "$(wt_of "$1" "$2")" config --local --get "$3"; }          # cfg <acct> <project> <key>
gitc() { local a="$1" p="$2"; shift 2; as "$a" git -C "$(wt_of "$a" "$p")" "$@"; }  # gitc <acct> <project> <git args>

# ------------------------------------------------------------- fixtures

git config --global user.name test; git config --global user.email t@example.invalid
git config --global init.defaultBranch main
W=/fixtures/work; mkdir -p "$W" /fixtures/http; git init -q -b main "$W"
echo hello > "$W/README.md"; git -C "$W" add -A && git -C "$W" commit -q -m first
for r in team other; do git clone -q --bare "$W" "/fixtures/http/$r.git"; git -C "/fixtures/http/$r.git" config http.receivepack true; done
git clone -q --bare "$W" /fixtures/local.git
advance_upstream() {   # a new commit on the team upstream, pushed from the filesystem
  rm -rf "$T/adv"; git clone -q /fixtures/http/team.git "$T/adv"
  echo "up $1" > "$T/adv/up.txt"; git -C "$T/adv" add -A && git -C "$T/adv" commit -q -m "upstream $1"
  git -C "$T/adv" push -q origin main
}
upstream_main() { git --git-dir /fixtures/http/team.git rev-parse main; }

# Every repo wants a valid token (root's or one account's) for read AND write. Each
# request is logged "<repo> <who> <fetch|push>"; who is the NAME of the token it
# carried, none, or bad.
cat > "$T/githttp.py" <<'PY'
import base64, os, subprocess, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
ROOT, LOG = sys.argv[1], sys.argv[2]
TOK = dict(p.split(':', 1)[::-1] for p in os.environ['TOKS'].split(','))
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
        kind = 'push' if 'receive-pack' in self.path else 'fetch'
        with open(LOG, 'a') as f:
            f.write('%s %s %s\n' % (repo, who, kind))
        n = int(self.headers.get('Content-Length') or 0)
        body = self.rfile.read(n) if n else b''
        if who in ('none', 'bad'):
            self.send_response(401)
            self.send_header('WWW-Authenticate', 'Basic realm="git"')
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        path, _, qs = self.path.partition('?')
        env = dict(os.environ, GIT_PROJECT_ROOT=ROOT, GIT_HTTP_EXPORT_ALL='1', PATH_INFO=path,
                   QUERY_STRING=qs, REQUEST_METHOD=self.command, REMOTE_USER=who,
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
TOKS="root:$ROOT_TOK,ada:$ADA_TOK,rex:$REX_TOK,eve:$EVE_TOK" python3 "$T/githttp.py" /fixtures/http "$REQLOG" >/dev/null 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 50); do (exec 3<>/dev/tcp/127.0.0.1/8099) 2>/dev/null && break; sleep 0.1; done
git ls-remote "$TEAM" >/dev/null 2>&1 && fail "fixture: team answered without a token"
git ls-remote "http://x:nope@127.0.0.1:8099/team.git" >/dev/null 2>&1 && fail "fixture: team accepted a bad token"
git ls-remote "http://x:$ADA_TOK@127.0.0.1:8099/team.git" >/dev/null 2>&1 || fail "fixture: team refused ada's token"
grep -q '^team ada fetch$' "$REQLOG" || fail "fixture: the server did not log ada's request"
pass "fixture: the http server wants a valid token and says whose it was (negative controls)"

# ---------------------------------------------------------------- profile helpers

install -d -m 0700 "$PROV" "$INSTALL"
export IKENGA_SYSTEMD_DIR="$SD" IKENGA_PROVISION_STABLE="$T/sbin/ikenga-provision" IKENGA_PROJECTS_LOCK="$T/projects.lock" INSTALL_DIR="$INSTALL"
export PATH="$T/bin:$PATH"
RC=0
prov() { RC=0; "$PROVISION" sync-accounts --profile "$PROFILE" "$@" >"$OUT" 2>&1 || RC=$?; }
refr() { RC=0; "$PROVISION" refresh-projects --profile "$PROFILE" "$@" >"$OUT" 2>&1 || RC=$?; }
mksecrets() {   # args = secret lines (after the standard set)
  printf '%s\n' "# fake values only" \
    "[root] ROOT_TOK=$ROOT_TOK" \
    "[ada] TEAM_TOK=$ADA_TOK" "[rex] TEAM_TOK=$REX_TOK" "[eve] TEAM_TOK=$EVE_TOK" \
    "[ada] TEAM_TOK2=$ADA_TOK" "[rex] TEAM_TOK2=$REX_TOK" "[eve] TEAM_TOK2=$EVE_TOK" "$@" > "$SECRETS"
  chmod 0600 "$SECRETS"
}
mkprofile() {   # args = profile lines
  { echo 'ACCOUNTS=(ada rex eve)'; echo 'AGENT_ACCOUNTS=(rex)'; echo 'PROJECTS_REFRESH_SCHEDULE=off'
    echo "SECRETS_FILE=$SECRETS"; echo 'PROJECTS_TOKEN_SECRET=ROOT_TOK'
    echo "PROJECTS=(\"team=$TEAM#main\" \"other=$URL/other.git#main\")"; printf '%s\n' "$@"; } > "$PROFILE"
  chmod 0600 "$PROFILE"
}
SHARED=('PROJECTS_PUSH_TOKEN_SECRETS=("team=TEAM_TOK")' 'PROJECTS_SHARED_MAIN=(team)')
mksecrets

# ---------------------------------------------------- 1. validation errors

bad() {   # bad <expected message fragment> <extra profile lines...>
  local want="$1"; shift
  mkprofile "$@"
  prov --dry-run
  [[ $RC -ne 0 ]] || fail "expected failure for: $want"
  grep -qF -- "$want" "$OUT" || fail "wanted '$want' in: $(cat "$OUT")"
  ! grep -qE 'FAKE-(root|ada|rex|eve)-token' "$OUT" || fail "a token value reached the output of: $want"
}
bad "PROJECTS_PUSH_TOKEN_SECRETS names 'teem', which is not in PROJECTS" 'PROJECTS_PUSH_TOKEN_SECRETS=("teem=TEAM_TOK")'
bad "PROJECTS_PUSH_TOKEN_SECRETS names 'team other', which is not in PROJECTS" 'PROJECTS_PUSH_TOKEN_SECRETS=("team other=TEAM_TOK")'
bad "PROJECTS_PUSH_TOKEN_SECRETS names 'team' twice" 'PROJECTS_PUSH_TOKEN_SECRETS=("team=TEAM_TOK" "team=TEAM_TOK2")'
bad "PROJECTS_PUSH_TOKEN_SECRETS entry 'team' must look like project=SECRET_NAME" 'PROJECTS_PUSH_TOKEN_SECRETS=("team")'
bad "PROJECTS_PUSH_TOKEN_SECRETS entry 'team=' must look like project=SECRET_NAME" 'PROJECTS_PUSH_TOKEN_SECRETS=("team=")'
bad "PROJECTS_PUSH_TOKEN_SECRETS 'team=IKENGA_TOK': 'IKENGA_TOK' is not a usable secret name" 'PROJECTS_PUSH_TOKEN_SECRETS=("team=IKENGA_TOK")'
bad "PROJECTS_PUSH_TOKEN_SECRETS 'team=has space': 'has space' is not a usable secret name" 'PROJECTS_PUSH_TOKEN_SECRETS=("team=has space")'
bad "PROJECTS_PUSH_TOKEN_SECRETS 'local': a push token only works for an https:// or http:// upstream" \
  'PROJECTS=("local=file:///fixtures/local.git")' 'PROJECTS_PUSH_TOKEN_SECRETS=("local=TEAM_TOK")'
bad "PROJECTS 'team': the URL carries credentials" 'PROJECTS=("team=http://u:p@127.0.0.1:8099/team.git")' 'PROJECTS_PUSH_TOKEN_SECRETS=("team=TEAM_TOK")'
bad "PROJECTS_SHARED_MAIN names 'teem', which is not in PROJECTS" 'PROJECTS_PUSH_TOKEN_SECRETS=("team=TEAM_TOK")' 'PROJECTS_SHARED_MAIN=(teem)'
bad "PROJECTS_SHARED_MAIN names 'team other', which is not in PROJECTS" 'PROJECTS_PUSH_TOKEN_SECRETS=("team=TEAM_TOK")' 'PROJECTS_SHARED_MAIN=("team other")'
bad "PROJECTS_SHARED_MAIN names 'team' twice" 'PROJECTS_PUSH_TOKEN_SECRETS=("team=TEAM_TOK")' 'PROJECTS_SHARED_MAIN=(team team)'
bad "PROJECTS_SHARED_MAIN 'team' needs a PROJECTS_PUSH_TOKEN_SECRETS entry" 'PROJECTS_SHARED_MAIN=(team)'
bad "PROJECTS_SHARED_MAIN 'team' needs a PROJECTS_PUSH_TOKEN_SECRETS entry" 'PROJECTS_PUSH_TOKEN_SECRETS=("other=TEAM_TOK")' 'PROJECTS_SHARED_MAIN=(team)'
# No SECRETS_FILE at all.
{ echo 'ACCOUNTS=(ada)'; echo "PROJECTS=(\"team=$TEAM\")"; echo 'PROJECTS_REFRESH_SCHEDULE=off'; echo 'PROJECTS_PUSH_TOKEN_SECRETS=("team=TEAM_TOK")'; } > "$PROFILE"
prov --dry-run
[[ $RC -ne 0 ]] && grep -qF "PROJECTS_PUSH_TOKEN_SECRETS needs SECRETS_FILE" "$OUT" || fail "no SECRETS_FILE was not refused"
# Secret checks (need the secrets loaded): missing, and not reaching EVERY account of the project.
bad "PROJECTS_PUSH_TOKEN_SECRETS 'team': 'NOPE_TOK' is not in SECRETS_FILE" 'PROJECTS_PUSH_TOKEN_SECRETS=("team=NOPE_TOK")'
mksecrets "[ada] ONLY_ADA_TOK=x" "[agents] ONLY_AGENT_TOK=x"
bad "PROJECTS_PUSH_TOKEN_SECRETS 'team': ONLY_ADA_TOK is not delivered to ik-rex" 'PROJECTS_PUSH_TOKEN_SECRETS=("team=ONLY_ADA_TOK")'
bad "PROJECTS_PUSH_TOKEN_SECRETS 'team': ONLY_AGENT_TOK is not delivered to ik-ada" 'PROJECTS_PUSH_TOKEN_SECRETS=("team=ONLY_AGENT_TOK")'
# ...but only the project's own accounts count: PROJECTS_MEMBERS narrows the set that must be reached.
mkprofile 'PROJECTS_MEMBERS=(rex)' 'PROJECTS_PUSH_TOKEN_SECRETS=("team=ONLY_AGENT_TOK")'
prov --dry-run
[[ $RC -eq 0 ]] || fail "a secret that reaches the whole (narrowed) member set was refused: $(cat "$OUT")"
# A restricted project's deploy token must not reach anyone outside the access list.
mksecrets "[everyone] HUB_TOK=x"
bad "PROJECTS_ACCESS 'team' is restricted, but its deploy token HUB_TOK is delivered to ik-ada, who is not on its access list" \
  'PROJECTS_ACCESS=("team=rex")' 'PROJECTS_TOKEN_SECRETS=("team=HUB_TOK")'
mksecrets "[root] HUB_TOK=x"
mkprofile 'PROJECTS_ACCESS=("team=rex")' 'PROJECTS_TOKEN_SECRETS=("team=HUB_TOK")'
prov --dry-run
[[ $RC -eq 0 ]] || fail "a root-scoped deploy token on a restricted project was refused: $(cat "$OUT")"
# ...and neither may its PUSH token: it reads the whole repo from GitHub just the same.
mksecrets "[everyone] PUSH_ALL=x"
bad "PROJECTS_ACCESS 'team' is restricted, but its push token PUSH_ALL is delivered to ik-ada, who is not on its access list" \
  'PROJECTS_ACCESS=("team=rex")' 'PROJECTS_PUSH_TOKEN_SECRETS=("team=PUSH_ALL")'
mksecrets "[rex] PUSH_MIXED=x" "[eve] PUSH_MIXED=y"
bad "PROJECTS_ACCESS 'team' is restricted, but its push token PUSH_MIXED is delivered to ik-eve, who is not on its access list" \
  'PROJECTS_ACCESS=("team=rex")' 'PROJECTS_PUSH_TOKEN_SECRETS=("team=PUSH_MIXED")'
# The per-account TEAM_TOK of mksecrets reaches ada and eve as well, so it is refused too.
mksecrets
bad "PROJECTS_ACCESS 'team' is restricted, but its push token TEAM_TOK is delivered to ik-ada" \
  'PROJECTS_ACCESS=("team=rex")' 'PROJECTS_PUSH_TOKEN_SECRETS=("team=TEAM_TOK")'
# A push token for an UNRESTRICTED project may reach everyone (the existing behaviour is unchanged).
mksecrets "[everyone] PUSH_ALL=x"
mkprofile 'PROJECTS_PUSH_TOKEN_SECRETS=("team=PUSH_ALL")'
prov --dry-run
[[ $RC -eq 0 ]] || fail "an everyone-scoped push token on an unrestricted project was refused: $(cat "$OUT")"
# A push token scoped to exactly the access list is accepted.
mksecrets "[rex] PUSH_REX=x"
mkprofile 'PROJECTS_ACCESS=("team=rex")' 'PROJECTS_PUSH_TOKEN_SECRETS=("team=PUSH_REX")'
prov --dry-run
[[ $RC -eq 0 ]] || fail "a push token scoped to exactly the access list was refused: $(cat "$OUT")"
mksecrets
pass "validation: typo'd / space-bypass / duplicate / malformed / unusable-name / non-http / credentialed-URL entries, shared-main without a token, a secret that is missing or does not reach every account, and a restricted project's deploy OR push token reaching outsiders all die before anything is written"
[[ ! -e $PROJ ]] || fail "a rejected profile created $PROJ"

# -------- 2. before: ada alone has an ordinary clone (her own branch, origin = the mirror)

mkprofile 'PROJECTS_MEMBERS=(ada)'
prov
[[ $RC -eq 0 ]] || fail "baseline exited $RC"
[[ "$(gitc ada team symbolic-ref --short HEAD)" == ada/main ]] || fail "baseline: ada is not on ada/main"
[[ "$(cfg ada team remote.origin.url)" == "$PROJ/team.git" ]] || fail "baseline: origin is not the mirror"
[[ "$(cfg ada team remote.origin.pushurl)" == "$TEAM" ]] || fail "baseline: the push URL is not the upstream"
! as ada git -C "$(wt_of ada team)" config --local --get-regexp '^credential' >/dev/null 2>&1 || fail "baseline: a clone made without PUSH_TOKEN_SECRETS has a credential helper"
[[ ! -e "$(wt_of rex team)" ]] || fail "baseline: PROJECTS_MEMBERS=(ada) still cloned for rex"
pass "baseline: without the new keys a clone is exactly what it was (own branch, origin = mirror, no credential helper)"

# ------------- 3. shared main + push tokens: dry run, then the real thing

mkprofile "${SHARED[@]}" 'PROJECTS_FASTFORWARD=(rex ada)'
prov --dry-run
[[ $RC -eq 0 ]] || fail "dry run exited $RC"
grep -qF 'project team: pushes use $TEAM_TOK from the account'"'"'s environment' "$OUT" || fail "dry run: no push-token line: $(cat "$OUT")"
grep -qF "project team: shared main: clones track origin/main at the upstream, not <account>/main" "$OUT" || fail "dry run: no shared-main line"
! grep -qF 'project other: ' "$OUT" || fail "dry run: claims something about the plain project"
grep -qF "would: clone ik-rex:team on main (shared main: origin is $TEAM) (push token env: TEAM_TOK)" "$OUT" || fail "dry run: rex's shared clone is not planned"
grep -qF "would: clone ik-ada:team: push credential helper (env TEAM_TOK)" "$OUT" || fail "dry run: ada's existing clone gets no helper plan"
grep -qF "ik-ada:team: made before PROJECTS_SHARED_MAIN" "$OUT" || fail "dry run: no note about ada's old-style clone"
[[ ! -e "$(wt_of rex team)" ]] || fail "the dry run created a clone"
! as ada git -C "$(wt_of ada team)" config --local --get-regexp '^credential' >/dev/null 2>&1 || fail "the dry run wrote a helper"
pass "dry run: shows the push secret NAME, the shared-main behaviour and each clone's plan, and writes nothing"

: > "$REQLOG"
prov
[[ $RC -eq 0 ]] || fail "shared run exited $RC"
for a in rex eve; do
  [[ "$(gitc $a team symbolic-ref --short HEAD)" == main ]] || fail "ik-$a: not on main"
  [[ "$(cfg $a team remote.origin.url)" == "$TEAM" ]] || fail "ik-$a: origin is '$(cfg $a team remote.origin.url)', not the upstream"
  ! as $a git -C "$(wt_of $a team)" config --local --get remote.origin.pushurl >/dev/null 2>&1 || fail "ik-$a: has a separate push URL"
  [[ "$(cfg $a team branch.main.remote)" == origin && "$(cfg $a team branch.main.merge)" == refs/heads/main ]] || fail "ik-$a: main does not track origin/main"
  [[ "$(gitc $a team rev-parse --abbrev-ref 'main@{upstream}')" == origin/main ]] || fail "ik-$a: main@{upstream} is not origin/main"
  [[ "$(gitc $a team rev-parse HEAD)" == "$(git --git-dir $PROJ/team.git rev-parse main)" ]] || fail "ik-$a: not at the mirror's main"
  grep -qF "$PROJ/team.git/objects" "$(wt_of $a team)/.git/objects/info/alternates" || fail "ik-$a: not sharing the mirror's objects"
done
# ada's old clone: not rewritten (branch, origin, push URL), but it gets the helper.
[[ "$(gitc ada team symbolic-ref --short HEAD)" == ada/main && "$(cfg ada team remote.origin.url)" == "$PROJ/team.git" ]] || fail "ada's existing clone was rewritten"
grep -qF "ik-ada:team: made before PROJECTS_SHARED_MAIN (origin is the mirror, on its own branch); left as it is" "$OUT" || fail "no note about ada's old-style clone"
# the plain project is untouched by all of this
[[ "$(gitc rex other symbolic-ref --short HEAD)" == rex/main && "$(cfg rex other remote.origin.url)" == "$PROJ/other.git" ]] || fail "the plain project's clone changed shape"
! as rex git -C "$(wt_of rex other)" config --local --get-regexp '^credential' >/dev/null 2>&1 || fail "the plain project's clone got a credential helper"
KEY="credential.$TEAM.helper"
for a in ada rex eve; do
  H="$(cfg $a team "$KEY")" || fail "ik-$a: no credential helper for the upstream"
  [[ "$H" == *'${TEAM_TOK:-}'* && "$H" == *'password=$TEAM_TOK'* ]] || fail "ik-$a: helper does not read the env var: $H"
  [[ "$(cfg $a team ikenga.pushTokenSecret)" == TEAM_TOK ]] || fail "ik-$a: no marker"
done
pass "shared main: new clones are on main tracking origin/main with origin = the upstream (no push URL); ada's old clone is left alone and noted; every clone has the helper"

# no token value in any config, file or log (the env file is the one place they are delivered to)
for tok in "${ALL_TOKS[@]}"; do
  ! grep -rIaq -e "$tok" "$PRINC" "$PROJ" /etc/gitconfig "$OUT" 2>/dev/null || fail "$tok was written somewhere under the homes, the mirrors, /etc/gitconfig or the run output: $(grep -rIal -e "$tok" "$PRINC" "$PROJ" /etc/gitconfig "$OUT" 2>/dev/null | head -3 | tr '\n' ' ')"
done
for a in ada rex eve; do as $a git -C "$(wt_of $a team)" config --local --list | grep -qE 'FAKE-' && fail "ik-$a: a token is in 'git config --list'" || true; done
grep -qF "TEAM_TOK=$REX_TOK" /etc/ikenga/secrets/ik-rex.env || fail "rex's own env file lacks TEAM_TOK (the delivery this relies on)"
! grep -q "$ADA_TOK" /etc/ikenga/secrets/ik-rex.env || fail "rex's env file holds ada's token"
pass "no token value in any clone config, home, mirror, /etc/gitconfig or the run output (only each account's own env file has its own)"

prov
[[ $RC -eq 0 ]] || fail "rerun exited $RC"
! grep -qE 'clone ik-|push credential helper' "$OUT" || fail "rerun changed something: $(cat "$OUT")"
pass "a rerun changes nothing"

# -------------- 4. the helper emits the env token, scoped to the upstream URL

fill() {   # fill <acct> <path> [VAR=value...] : what git would get from the clone's helpers
  local a="$1" path="$2"; shift 2
  printf 'protocol=http\nhost=127.0.0.1:8099\npath=%s\n\n' "$path" | as "$a" "$@" git -C "$(wt_of "$a" team)" credential fill 2>/dev/null
}
for a in ada rex eve; do
  tokvar=$(echo "${a^^}_TOK")
  out="$(fill $a team.git TEAM_TOK="${!tokvar}")"
  [[ "$out" == *"username=x-access-token"* && "$out" == *"password=${!tokvar}"* ]] || fail "ik-$a: the helper did not emit the env token: $out"
done
out="$(fill rex team.git)" && fail "the helper produced a credential with TEAM_TOK unset: $out" || true
out="$(fill rex other.git TEAM_TOK=$REX_TOK)" && fail "the helper was offered to a DIFFERENT repo on the same host: $out" || true
out="$(printf 'protocol=https\nhost=github.com\npath=x/team.git\n\n' | as rex TEAM_TOK=$REX_TOK git -C "$(wt_of rex team)" credential fill 2>/dev/null)" && fail "the helper was offered to a different host: $out" || true
pass "the helper prints the account's env token for the upstream URL only; nothing when the variable is unset, for another repo, or for another host"

# --------------- 5. a real push, with git using that helper, over HTTP

: > "$REQLOG"
# ada, from her OLD-style clone (her own branch, push URL = upstream): the helper path for a clone made before
gitc ada team commit -q --allow-empty -m "ada was here"
gitc ada team push -q origin ada/main:main 2>/dev/null && fail "ada pushed with no TEAM_TOK in her environment" || true
! grep -q '^team ada push$' "$REQLOG" || fail "a push with no token reached the server authenticated"
as ada TEAM_TOK="$ADA_TOK" git -C "$(wt_of ada team)" push -q origin ada/main:main || fail "ada's push with TEAM_TOK failed"
grep -q '^team ada push$' "$REQLOG" || fail "the server never saw ada's push: $(cat "$REQLOG")"
! grep '^team ' "$REQLOG" | grep -vqE ' (none|ada) ' || fail "a push used a token that is not ada's: $(sort -u "$REQLOG" | tr '\n' ' ')"
[[ "$(upstream_main)" == "$(gitc ada team rev-parse HEAD)" ]] || fail "the upstream did not get ada's commit"
pass "push: git used the helper (env TEAM_TOK of the session) and the server saw ada's token on the receive-pack; without the variable it was refused"

# the mirror is stale on purpose now. rex (shared clone) must see the upstream, not the mirror.
[[ "$(git --git-dir $PROJ/team.git rev-parse main)" != "$(upstream_main)" ]] || fail "test setup: the mirror is not behind"
: > "$REQLOG"
gitc rex team commit -q --allow-empty -m "rex local"
REJ=0; PUSHOUT="$(as rex TEAM_TOK="$REX_TOK" git -C "$(wt_of rex team)" push origin main 2>&1)" || REJ=1
[[ $REJ -eq 1 ]] || fail "rex's push from behind the upstream was accepted"
grep -qiE 'rejected|non-fast-forward|fetch first' <<<"$PUSHOUT" || fail "the push failed, but not as a non-fast-forward rejection: $PUSHOUT"
as rex TEAM_TOK="$REX_TOK" git -C "$(wt_of rex team)" fetch -q origin || fail "rex's fetch of origin failed"
grep -q '^team rex fetch$' "$REQLOG" || fail "rex's fetch of origin never reached the upstream (it read the mirror)"
[[ "$(gitc rex team rev-parse origin/main)" == "$(upstream_main)" ]] || fail "after 'git fetch origin' rex does not see the upstream's main"
gitc rex team rebase -q origin/main || fail "rebase"
as rex TEAM_TOK="$REX_TOK" git -C "$(wt_of rex team)" push -q origin main || fail "rex's push after fetching origin failed"
[[ "$(upstream_main)" == "$(gitc rex team rev-parse HEAD)" ]] || fail "the upstream did not get rex's commit"
grep -q '^team rex push$' "$REQLOG" || fail "the server never saw rex's push"
! grep -q '^team ada ' "$REQLOG" || fail "rex used ada's token"
[[ "$(git --git-dir $PROJ/team.git rev-parse main)" != "$(upstream_main)" ]] || fail "test setup: the mirror caught up on its own"
pass "shared main: origin fetch is the upstream (the stale mirror did not matter): rejected from behind, fetch origin, rebase, push main"

# -------- 6. refresh-projects: fast-forward from the upstream, as each account

advance_upstream 1
sample_argv() { ( while :; do grep -alE 'FAKE-(root|ada|rex|eve)-toke[n]' /proc/[0-9]*/cmdline 2>/dev/null || true; done > "$1" ) & SAMPLER=$!; }
sample_argv "$T/control-hits"
bash -c 'sleep 1; true' FAKE-rex-token-222 &
CONTROL=$!
sleep 0.6; kill "$SAMPLER" 2>/dev/null || true; wait "$SAMPLER" "$CONTROL" 2>/dev/null || true
[[ -s "$T/control-hits" ]] || fail "argv sampler control: a planted token was not seen"

# a dirty clone first
echo scratch > "$(wt_of rex team)/untracked.txt"
OLD_REX="$(gitc rex team rev-parse HEAD)"; OLD_EVE="$(gitc eve team rev-parse HEAD)"
: > "$REQLOG"
sample_argv "$T/argv-hits"
refr
kill "$SAMPLER" 2>/dev/null || true; wait "$SAMPLER" 2>/dev/null || true
[[ $RC -eq 0 ]] || fail "refresh (dirty) exited $RC"
[[ "$(gitc rex team rev-parse HEAD)" == "$OLD_REX" ]] || fail "a dirty shared-main clone was fast-forwarded"
grep -qF "ik-rex:team: has uncommitted or untracked changes; left as it is" "$OUT" || fail "no dirty note"
grep -q '^team rex fetch$' "$REQLOG" || fail "refresh did not fetch the upstream as rex"
rm -f "$(wt_of rex team)/untracked.txt"

# on another branch: left
gitc rex team checkout -q -b feature
: > "$REQLOG"
refr
grep -qF "ik-rex:team: on feature, not main; left as it is" "$OUT" || fail "no off-branch note: $(cat "$OUT")"
[[ "$(gitc rex team rev-parse HEAD)" == "$OLD_REX" ]] || fail "a clone on another branch moved"
gitc rex team checkout -q main

: > "$REQLOG"
sample_argv "$T/argv-hits"
refr
kill "$SAMPLER" 2>/dev/null || true; wait "$SAMPLER" 2>/dev/null || true
[[ $RC -eq 0 ]] || fail "refresh exited $RC"
[[ "$(gitc rex team rev-parse HEAD)" == "$(upstream_main)" ]] || fail "rex (shared main, in FASTFORWARD, clean, behind) was not fast-forwarded to the UPSTREAM's main (mirror: $(git --git-dir $PROJ/team.git rev-parse main), upstream: $(upstream_main))"
grep -qF "clone ik-rex:team main fast-forwarded" "$OUT" || fail "no fast-forward line"
[[ "$(gitc eve team rev-parse HEAD)" == "$OLD_EVE" ]] || fail "eve is not in PROJECTS_FASTFORWARD but her clone moved"
! grep -q '^team eve ' "$REQLOG" || fail "refresh fetched as eve"
grep -q '^team rex fetch$' "$REQLOG" || fail "no authenticated fetch as rex"
! grep '^team ' "$REQLOG" | grep -qE ' (ada|eve) ' || fail "refresh used a token of an account that is not being fast-forwarded: $(sort -u "$REQLOG" | tr '\n' ' ')"
# ada's pre-shared clone still follows the mirror (which refresh_mirrors just updated with root's token)
[[ "$(gitc ada team rev-parse HEAD)" == "$(upstream_main)" ]] || fail "ada's old-style clone (follows the mirror) was not fast-forwarded: ada $(gitc ada team rev-parse HEAD) upstream $(upstream_main)"
[[ ! -s "$T/argv-hits" ]] || fail "a token appeared in a process argv during refresh-projects: $(cat "$T/argv-hits" | tr '\0' ' ' | head -c 300)"
for tok in "${ALL_TOKS[@]}"; do ! grep -rIaq -e "$tok" "$PRINC" "$PROJ" /etc/gitconfig "$OUT" 2>/dev/null || fail "$tok was written somewhere by refresh-projects"; done
! find "$T" -name 'pushtok*' | grep -q . || fail "the account's token file was left in the scratch area"
pass "refresh-projects: a shared-main clone is fast-forwarded from the UPSTREAM as its own account (clean, on main, strictly behind only), with that account's token via an inherited descriptor; no token in argv, files or output"

refr --dry-run
grep -qF "[dry-run] ik-rex:team: would fetch the upstream $TEAM as the account (its own TEAM_TOK)" "$OUT" || fail "refresh dry run does not describe the upstream fetch: $(cat "$OUT")"
pass "refresh-projects --dry-run describes the upstream fetch and does it not"

# ------------------ 7. converge: never touch what the account set itself

KEYQ="$KEY"
# eve removes the helper: it stays removed
as eve git -C "$(wt_of eve team)" config --local --unset-all "$KEYQ"
# rex swaps in her own
as rex git -C "$(wt_of rex team)" config --local --replace-all "$KEYQ" '!f() { echo username=me; echo password=mine; }; f'
OWN="$(cfg rex team "$KEYQ")"
prov
[[ $RC -eq 0 ]] || fail "converge run exited $RC"
! as eve git -C "$(wt_of eve team)" config --local --get "$KEYQ" >/dev/null 2>&1 || fail "the helper eve removed was put back"
grep -qF "ik-eve:team: the push credential helper was removed from the clone; left removed" "$OUT" || fail "no note about eve's removed helper"
[[ "$(cfg rex team "$KEYQ")" == "$OWN" ]] || fail "rex's own helper was replaced"
grep -qF "ik-rex:team: the clone has its own credential helper for $TEAM; left as it is" "$OUT" || fail "no note about rex's own helper"
# ada's is ours: a rotated secret name in the profile updates it (and the marker)
mkprofile 'PROJECTS_PUSH_TOKEN_SECRETS=("team=TEAM_TOK2")' 'PROJECTS_SHARED_MAIN=(team)'
prov
[[ $RC -eq 0 ]] || fail "rotation run exited $RC"
[[ "$(cfg ada team "$KEYQ")" == *'${TEAM_TOK2:-}'* && "$(cfg ada team ikenga.pushTokenSecret)" == TEAM_TOK2 ]] || fail "ada's own-made helper was not updated to TEAM_TOK2: $(cfg ada team "$KEYQ")"
grep -qF "clone ik-ada:team: push credential helper (env TEAM_TOK2)" "$OUT" || fail "no change line for ada's rotation"
[[ "$(cfg rex team "$KEYQ")" == "$OWN" ]] || fail "rotation replaced rex's own helper"
pass "converge: a helper the account removed stays removed, one the account set itself is kept, one we set follows a renamed secret"

echo "==> [Container] all PROJECTS_PUSH_TOKEN_SECRETS / PROJECTS_SHARED_MAIN checks PASSED"
