#!/bin/bash
# provision.sh sync-accounts: shared project clones (D-B2) and scoped secrets
# (D-B3), run as root inside an ubuntu:24.04 container
# (test-accounts-container.sh). Four accounts (ada, grace = people; rex, ruby =
# agents) plus one unmanaged ik-eve, a local bare repo as the project, a
# loopback HTTP server for the private-repo path, and a fake secrets file.
#
# Every secret value below starts with FAKE- (or is the one deliberately odd
# "everyone" value). The suite asserts none of them ever reaches the output.
set -euo pipefail

T="$(mktemp -d)"
PROV=/root/prov
PROVISION="/work/provision.sh"
PROFILE="$PROV/profile.env"
SECRETS="$PROV/secrets"
OUT="$T/out"
ALL="$T/all-output"
PROJ=/srv/ikenga/projects
SDIR=/etc/ikenga/secrets
: > "$ALL"

fail() { echo "FAIL: $*" >&2; [[ -f "$OUT" ]] && { echo "--- last output ---" >&2; cat "$OUT" >&2; }; exit 1; }
pass() { echo "==> [Container] $* PASSED"; }
ok() { local d="$1"; shift; "$@" || fail "$d"; }

SHARED_VALUE='FAKE-everyone value=with = signs # and  spaces'

# ------------------------------------------------------------------ host

PRINC=/var/lib/ikenga-test/principals
mkacct() {   # name uid : mimics what `ikenga-server accounts create` leaves behind
  groupadd -g "$2" "ik-$1"
  useradd -u "$2" -g "$2" -M -d "$PRINC/$2/home" -s /bin/sh "ik-$1"
  install -d -m 0700 -o "$2" -g "$2" "$PRINC/$2/home"
}
mkdir -p "$PRINC"
mkacct ada 20001; mkacct grace 20002; mkacct rex 20003; mkacct ruby 20004; mkacct eve 20005
# A system user that must never be touched (the admin).
useradd -m ops

# Run as an account the way the daemon's sessions run: uid + private gid, NO
# supplementary groups (src-tauri/src/executor/t1.rs verify_dropped()).
as() {
  local n="$1" u; shift
  u="$(id -u "ik-$n")"
  ( cd / && setpriv --reuid="$u" --regid="$u" --clear-groups \
      env -i HOME="$PRINC/$u/home" PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 "$@" )
}

# ------------------------------------------------------------- fixtures

mkdir -p /fixtures/http
git config --global user.name test; git config --global user.email t@example.invalid
git init -q -b main /fixtures/work
echo "hello" > /fixtures/work/README.md
git -C /fixtures/work add README.md && git -C /fixtures/work commit -q -m "first"
git clone -q --bare /fixtures/work /fixtures/app.git
git clone -q --bare /fixtures/work /fixtures/http/private.git

# A git smart-HTTP server (git http-backend) that insists on Basic auth with a
# fake deploy token, so the private-repo path is exercised for real.
cat > "$T/githttp.py" <<'PY'
import base64, os, subprocess, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
ROOT, TOKEN = sys.argv[1], os.environ['FIXTURE_TOKEN']
class H(BaseHTTPRequestHandler):
    def go(self):
        auth = self.headers.get('Authorization', '')
        good = False
        if auth.startswith('Basic '):
            try:
                _, _, p = base64.b64decode(auth[6:]).decode().partition(':')
                good = (p == TOKEN)
            except Exception:
                pass
        n = int(self.headers.get('Content-Length') or 0)
        body = self.rfile.read(n) if n else b''
        if not good:
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
FIXTURE_TOKEN=FAKE-deploy-token-444 python3 "$T/githttp.py" /fixtures/http >/dev/null 2>&1 &
HTTP_PID=$!
trap 'kill "$HTTP_PID" 2>/dev/null || true; rm -rf "$T"' EXIT
for _ in $(seq 1 50); do (exec 3<>/dev/tcp/127.0.0.1/8099) 2>/dev/null && break; sleep 0.1; done
git ls-remote http://127.0.0.1:8099/private.git >/dev/null 2>&1 && fail "fixture: private repo answered without a token"
pass "fixture: private repo refuses an anonymous clone"

install -d -m 0700 "$PROV"
cat > "$PROFILE" <<'EOF'
ACCOUNTS=(ada grace rex ruby)
AGENT_ACCOUNTS=(rex ruby)
PROJECTS=("app=file:///fixtures/app.git" "private=http://127.0.0.1:8099/private.git#main")
PROJECTS_TOKEN_SECRET=GIT_DEPLOY_TOKEN
SECRETS_FILE=/root/prov/secrets
EOF
chmod 0600 "$PROFILE"
write_secrets() {   # args: lines
  printf '%s\n' "# fake values only" "" "$@" > "$SECRETS"
  chmod 0600 "$SECRETS"
}
BASE_SECRETS=(
  "[everyone] SHARED_NOTE=$SHARED_VALUE"
  "[agents] AGENT_KEY=FAKE-agents-key-111"
  "[rex] REX_ONLY=FAKE-rex-only-222"
  "[ada,grace] HUMANS_KEY=FAKE-humans-333"
  "[root] GIT_DEPLOY_TOKEN=FAKE-deploy-token-444"
  "[ruby] RUBY_ONLY=FAKE-ruby-only-555"
)
write_secrets "${BASE_SECRETS[@]}"

RC=0
prov() { RC=0; "$PROVISION" sync-accounts --profile "$PROFILE" "$@" >"$OUT" 2>&1 || RC=$?; cat "$OUT" >> "$ALL"; }
keys_of() { sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' "$1" | sort | tr '\n' ' '; }

# ------------------------------------------------- 1. dry run changes nothing

prov --dry-run
[[ $RC -eq 0 ]] || fail "dry run exited $RC"
grep -q 'would: worktree ik-rex:app' "$OUT" || fail "dry run did not plan rex's worktree"
grep -q 'would: secrets ik-rex: +AGENT_KEY +REX_ONLY +SHARED_NOTE' "$OUT" || fail "dry run did not plan rex's secrets by name"
! getent group ikenga-projects >/dev/null || fail "dry run created the group"
[[ ! -e $PROJ && ! -e $SDIR ]] || fail "dry run created directories"
grep -q 'FAKE-' "$OUT" && fail "dry run printed a secret value"
pass "dry run plans group, worktrees and per-account secret names, and changes nothing"

# --------------------------------------------- 2. first run (with a sampler)

# Watch every process's argv for the deploy token while the first run clones
# the private repo. (The pattern is written so this grep's own argv cannot
# match it.)
sample_argv() { ( while :; do grep -al 'FAKE-deploy-tok[e]n' /proc/[0-9]*/cmdline 2>/dev/null || true; done > "$1" ) & SAMPLER=$!; }
# Control: the sampler must be able to see a token in argv at all.
sample_argv "$T/control-hits"
bash -c 'sleep 1; true' FAKE-deploy-token-444 &
CONTROL=$!
sleep 0.6; kill "$SAMPLER" 2>/dev/null || true; wait "$SAMPLER" "$CONTROL" 2>/dev/null || true
[[ -s "$T/control-hits" ]] || fail "argv sampler control: a planted token was not seen"
sample_argv "$T/argv-hits"
prov
kill "$SAMPLER" 2>/dev/null || true; wait "$SAMPLER" 2>/dev/null || true
[[ $RC -eq 0 ]] || fail "first run exited $RC"
[[ ! -s "$T/argv-hits" ]] || fail "the deploy token appeared in a process argv"
pass "first run succeeded; deploy token never appeared in any argv"

# ------------------------------------------------- 3. group, folder, clones

getent group ikenga-projects >/dev/null || fail "group missing"
members=",$(getent group ikenga-projects | cut -d: -f4),"
for a in ada grace rex ruby; do [[ "$members" == *",ik-$a,"* ]] || fail "ik-$a not in the group"; done
[[ "$members" != *",ik-eve,"* && "$members" != *",ops,"* ]] || fail "an unmanaged user joined the group"
[[ "$(stat -c '%U %G %a' $PROJ)" == "root ikenga-projects 2775" ]] || fail "projects dir is $(stat -c '%U %G %a' $PROJ)"
for p in app private; do
  [[ -d $PROJ/$p.git ]] || fail "no shared clone $p"
  [[ "$(git -C $PROJ/$p.git config core.sharedRepository)" =~ ^(1|group|true)$ ]] || fail "$p: core.sharedRepository not group"
  [[ "$(stat -c %G $PROJ/$p.git)" == ikenga-projects ]] || fail "$p.git is not group-owned"
done
[[ "$(git -C $PROJ/private.git config remote.origin.url)" == "http://127.0.0.1:8099/private.git" ]] || fail "private remote URL carries something it should not"
! grep -rqa 'FAKE-deploy-token' $PROJ || fail "the deploy token was written into a shared clone"
! grep -rqa 'FAKE-deploy-token' /etc/gitconfig 2>/dev/null || fail "the deploy token is in /etc/gitconfig"
grep -q 'directory = /srv/ikenga/projects/app.git' /etc/gitconfig || fail "safe.directory for app missing"
! grep -Eq 'directory *= *\*' /etc/gitconfig || fail "safe.directory is a wildcard"
pass "group, setgid 2775 folder, bare shared clones (public and token-authenticated), scoped safe.directory"

# ------------------------------------------------------------- 4. worktrees

for a in ada grace rex ruby; do
  u="$(id -u ik-$a)"
  for p in app private; do
    wt="$PRINC/$u/home/projects/$p"
    [[ -f "$wt/README.md" ]] || fail "ik-$a: no checkout of $p"
    [[ "$(stat -c %u "$wt")" == "$u" ]] || fail "ik-$a: worktree $p is not owned by the account"
    [[ "$(as "$a" git -C "$wt" branch --show-current)" == "$a/main" ]] || fail "ik-$a:$p is on '$(as "$a" git -C "$wt" branch --show-current)'"
  done
done
[[ ! -e "$(getent passwd ik-eve | cut -d: -f6)/projects" ]] || fail "unmanaged ik-eve got a worktree"
pass "each of 4 accounts has its own worktree of both projects on <account>/main; unmanaged ik-eve none"

# ------------------------------- 5. daemon-style sessions can use the clone

# Sessions have no supplementary groups: only the ACL lets them in. Prove
# every account can commit, and read the others' commits.
for a in ada grace rex ruby; do
  wt="$PRINC/$(id -u ik-$a)/home/projects/app"
  as "$a" git -C "$wt" -c user.name="$a" -c user.email="$a@example.invalid" commit -q --allow-empty -m "work by $a" \
    || fail "ik-$a cannot commit through the shared clone with no supplementary groups"
done
rexwt="$PRINC/$(id -u ik-rex)/home/projects/app"
as rex git -C "$rexwt" log --oneline -1 ada/main | grep -q 'work by ada' || fail "rex cannot read ada's commit"
as ada git -C "$PRINC/$(id -u ik-ada)/home/projects/app" log --oneline -1 grace/main | grep -q 'work by grace' || fail "ada cannot read grace's commit"
as ruby git -C "$PRINC/$(id -u ik-ruby)/home/projects/app" worktree list | grep -q "/rex/\|$(id -u ik-rex)" || fail "ruby cannot list the shared worktrees"
pass "all accounts commit via the shared clone and read each other's branches (no supplementary groups)"

# Negative control: the group alone is not enough. Strip the ACLs, show the
# commit fail, converge, show it work.
setfacl -R -b $PROJ/app.git
if as grace git -C "$PRINC/$(id -u ik-grace)/home/projects/app" -c user.name=g -c user.email=g@example.invalid commit -q --allow-empty -m "no acl" 2>/dev/null; then
  fail "negative control: commit worked without the ACL (the test proves nothing)"
fi
prov
[[ $RC -eq 0 ]] || fail "converge after ACL loss exited $RC"
grep -q 'ACL for ik-grace on /srv/ikenga/projects/app.git' "$OUT" || fail "the ACL was not restored by the run"
as grace git -C "$PRINC/$(id -u ik-grace)/home/projects/app" -c user.name=g -c user.email=g@example.invalid commit -q --allow-empty -m "acl back" \
  || fail "commit still fails after the ACL was restored"
pass "negative control: without ACLs the group alone cannot write (no supplementary groups); a rerun restores them"

# ------------------------------------------------------------------ secrets

gid_of() { id -g "ik-$1"; }
for a in ada grace rex ruby; do
  f="$SDIR/ik-$a.env"
  [[ -f $f ]] || fail "no secrets file for $a"
  [[ "$(stat -c '%u:%g %a' $f)" == "0:$(gid_of $a) 640" ]] || fail "$f is $(stat -c '%u:%g %a' $f)"
done
[[ ! -e "$SDIR/ik-eve.env" ]] || fail "unmanaged eve got a secrets file"
[[ "$(keys_of $SDIR/ik-ada.env)"   == "HUMANS_KEY SHARED_NOTE " ]] || fail "ada: $(keys_of $SDIR/ik-ada.env)"
[[ "$(keys_of $SDIR/ik-grace.env)" == "HUMANS_KEY SHARED_NOTE " ]] || fail "grace: $(keys_of $SDIR/ik-grace.env)"
[[ "$(keys_of $SDIR/ik-rex.env)"   == "AGENT_KEY REX_ONLY SHARED_NOTE " ]] || fail "rex: $(keys_of $SDIR/ik-rex.env)"
[[ "$(keys_of $SDIR/ik-ruby.env)"  == "AGENT_KEY RUBY_ONLY SHARED_NOTE " ]] || fail "ruby: $(keys_of $SDIR/ik-ruby.env)"
! grep -rqa GIT_DEPLOY_TOKEN $SDIR || fail "the root-only deploy token was delivered to an account"
pass "per-account files are root:<account gid> 0640 and hold exactly the secrets scoped to that account"

as ada cat $SDIR/ik-ada.env >/dev/null || fail "ada cannot read her own file"
! as ada cat $SDIR/ik-rex.env >/dev/null 2>&1 || fail "ada can read rex's secrets file"
! as ada sh -c "echo x >> $SDIR/ik-ada.env" 2>/dev/null || fail "ada can write her own secrets file"
! as rex cat $SDIR/ik-ruby.env >/dev/null 2>&1 || fail "rex can read ruby's secrets file"
pass "an account reads its own file, cannot read another's, cannot write its own"

# What the account's processes actually see.
[[ "$(as rex sh -lc 'printf %s "$SHARED_NOTE"')" == "$SHARED_VALUE" ]] || fail "a value with =, # and spaces did not survive"
[[ "$(as rex sh -lc 'printf %s "$REX_ONLY"')" == FAKE-rex-only-222 ]] || fail "rex login shell lacks REX_ONLY"
[[ "$(as rex sh -lc 'printf %s "$AGENT_KEY"')" == FAKE-agents-key-111 ]] || fail "rex login shell lacks the agents secret"
[[ "$(as rex bash -ic 'printf %s "$REX_ONLY"' 2>/dev/null)" == FAKE-rex-only-222 ]] || fail "rex interactive bash lacks REX_ONLY"
[[ "$(as ada sh -lc 'printf %s "${HUMANS_KEY-}"')" == FAKE-humans-333 ]] || fail "ada login shell lacks HUMANS_KEY"
for v in REX_ONLY AGENT_KEY RUBY_ONLY GIT_DEPLOY_TOKEN; do
  [[ -z "$(as ada sh -lc "printf %s \"\${$v-}\"")" ]] || fail "ada's login shell sees $v"
  [[ -z "$(as ada bash -ic "printf %s \"\${$v-}\"" 2>/dev/null)" ]] || fail "ada's interactive bash sees $v"
done
[[ -z "$(as rex sh -lc 'printf %s "${RUBY_ONLY-}${HUMANS_KEY-}"')" ]] || fail "rex sees ruby's or the humans' secret"
# Known gap, recorded so it cannot be quietly forgotten: a process that is
# neither a login shell nor interactive bash (an engine CLI exec'd directly, a
# Chi run) is not started through any file the provisioner controls.
if [[ -n "$(as rex sh -c 'printf %s "${REX_ONLY-}"')" ]]; then fail "unexpected: rex's non-login sh sees the secret (the documented gap closed? update README)"; fi
pass "login shells and interactive bash see exactly their account's secrets (value with '=', '#', spaces intact); KNOWN GAP confirmed: non-login non-shell processes see none"

! grep -q 'FAKE-\|with = signs' "$ALL" || fail "a secret value reached the provision output"
pass "no secret value in any provision output so far"

# -------------------------------------------------------------- idempotence

M1="$(stat -c %Y $SDIR/ik-rex.env)"; H1="$(sha256sum $SDIR/*.env | sha256sum)"
HEAD1="$(as rex git -C $rexwt rev-parse HEAD)"
sleep 1.1
prov
[[ $RC -eq 0 ]] || fail "rerun exited $RC"
grep -q 'no changes: the host already matches this profile' "$OUT" || fail "rerun reported changes"
[[ "$(stat -c %Y $SDIR/ik-rex.env)" == "$M1" && "$(sha256sum $SDIR/*.env | sha256sum)" == "$H1" ]] || fail "rerun rewrote a secrets file"
[[ "$(as rex git -C $rexwt rev-parse HEAD)" == "$HEAD1" ]] || fail "rerun touched a worktree"
pass "rerun with nothing new: 'no changes', secrets files and worktrees untouched"

# --------------------------------------------------------- upstream advances

echo more >> /fixtures/work/README.md
git -C /fixtures/work commit -q -am "second" && git -C /fixtures/work push -q /fixtures/app.git main
UPSTREAM="$(git -C /fixtures/work rev-parse HEAD)"
prov
[[ $RC -eq 0 ]] || fail "run after an upstream commit exited $RC"
grep -q 'project app fetched new commits' "$OUT" || fail "new upstream commit not reported"
[[ "$(git -C $PROJ/app.git rev-parse refs/remotes/origin/main)" == "$UPSTREAM" ]] || fail "shared clone did not fetch"
[[ "$(as rex git -C $rexwt rev-parse HEAD)" == "$HEAD1" ]] || fail "a fetch moved rex's worktree"
prov; grep -q 'no changes' "$OUT" || fail "second run after the fetch reported changes"
pass "an upstream commit is fetched into the shared clone only; worktrees stay where they are; next run is clean"

# ----------------------------------------------- scope narrowing, rotation

write_secrets \
  "[everyone] SHARED_NOTE=$SHARED_VALUE" \
  "[agents] AGENT_KEY=FAKE-agents-key-ROTATED" \
  "[ada] HUMANS_KEY=FAKE-humans-333" \
  "[root] GIT_DEPLOY_TOKEN=FAKE-deploy-token-444" \
  "[ruby] RUBY_ONLY=FAKE-ruby-only-555"
cp -a $SDIR/ik-grace.env "$T/grace.before"
prov --dry-run
grep -q 'would: secrets ik-grace: -HUMANS_KEY' "$OUT" || fail "dry run did not plan grace's narrowing"
grep -q 'would: secrets ik-rex: ~AGENT_KEY -REX_ONLY' "$OUT" || fail "dry run did not plan rex's changes"
cmp -s $SDIR/ik-grace.env "$T/grace.before" || fail "dry run changed grace's file"
prov
[[ $RC -eq 0 ]] || fail "narrowing run exited $RC"
grep -q 'secrets ik-grace: -HUMANS_KEY' "$OUT" || fail "summary lacks grace's removal"
grep -q 'secrets ik-rex: ~AGENT_KEY -REX_ONLY' "$OUT" || fail "summary lacks rex's rotation and removal"
grep -q 'secrets ik-ruby: ~AGENT_KEY' "$OUT" || fail "summary lacks ruby's rotation"
[[ "$(keys_of $SDIR/ik-grace.env)" == "SHARED_NOTE " ]] || fail "grace still has $(keys_of $SDIR/ik-grace.env)"
[[ "$(keys_of $SDIR/ik-rex.env)" == "AGENT_KEY SHARED_NOTE " ]] || fail "rex still has $(keys_of $SDIR/ik-rex.env)"
[[ -z "$(as grace sh -lc 'printf %s "${HUMANS_KEY-}"')" ]] || fail "grace's next login still sees HUMANS_KEY"
[[ "$(as rex sh -lc 'printf %s "$AGENT_KEY"')" == FAKE-agents-key-ROTATED ]] || fail "rex did not get the rotated value"
pass "narrowing a scope and deleting a line remove the secret from the account; rotation is applied; summary lists NAMES per account"

# Backups of the replaced files are root-only (an account-readable backup
# would undo the narrowing).
bk="$(ls /etc/ikenga/secrets-backup/ik-grace.env.bak-* | head -1)"
[[ -n "$bk" ]] || fail "no backup of grace's replaced file"
[[ "$(stat -c '%u:%g %a' "$bk")" == "0:0 600" && "$(stat -c '%a' /etc/ikenga/secrets-backup)" == 700 ]] || fail "backup is not root-only"
grep -q '^HUMANS_KEY=' "$bk" || fail "backup lacks the old content"
! as grace cat "$bk" >/dev/null 2>&1 || fail "grace can read the backup holding the secret she lost"
pass "replaced files are backed up root-only (0700 dir, 0600 files)"

prov; grep -q 'no changes' "$OUT" || fail "run after narrowing is not clean"
! grep -q 'FAKE-\|with = signs' "$ALL" || fail "a secret value reached the provision output"
pass "run after narrowing is clean; still no secret value in any output"

# ------------------------------------------------------------ safety checks

chmod 0644 "$SECRETS"
cp -a $SDIR/ik-rex.env "$T/rex.before"
prov
[[ $RC -ne 0 ]] || fail "ran with a world-readable SECRETS_FILE"
grep -q 'must be 0600' "$OUT" || fail "no clear refusal for loose permissions"
cmp -s $SDIR/ik-rex.env "$T/rex.before" || fail "a refused run changed a file"
chmod 0600 "$SECRETS"
pass "a SECRETS_FILE looser than 0600 is refused and nothing changes"

cp "$SECRETS" "$T/secrets.good"
printf '%s\n' '[rez] TYPO=FAKE-typo-666' >> "$SECRETS"
prov --dry-run
[[ $RC -ne 0 ]] && grep -q "not a managed account" "$OUT" || fail "a typo'd scope was not refused"
cp "$T/secrets.good" "$SECRETS"
printf '%s\n' 'NOSCOPE=FAKE-noscope-777' >> "$SECRETS"
prov --dry-run
[[ $RC -ne 0 ]] && grep -q "expected '\[scope\] NAME=value'" "$OUT" || fail "an unscoped secret was not refused"
cp "$T/secrets.good" "$SECRETS"
printf '%s\n' '[everyone] LD_PRELOAD=FAKE-evil-888' >> "$SECRETS"
prov --dry-run
[[ $RC -ne 0 ]] && grep -q "not an allowed secret name" "$OUT" || fail "a shell-sensitive name was not refused"
cp "$T/secrets.good" "$SECRETS"
printf '%s\n' '[ada] SHARED_NOTE=FAKE-dup-999' >> "$SECRETS"
prov --dry-run
[[ $RC -ne 0 ]] && grep -q "overlapping scopes" "$OUT" || fail "an overlapping duplicate was not refused"
cp "$T/secrets.good" "$SECRETS"; chmod 0600 "$SECRETS"
! grep -q 'FAKE-\|with = signs' "$ALL" || fail "an error message echoed a secret value"
pass "typo'd scope, unscoped line, shell-sensitive name and overlapping duplicate are refused without echoing values"

# ------------------------------------------- membership follows the profile

sed -i 's/^ACCOUNTS=.*/ACCOUNTS=()/' "$PROFILE"           # discover ik-* users in the uid range
prov --dry-run
[[ $RC -eq 0 ]] || fail "discovery dry run exited $RC"
grep -q 'managed accounts: .*eve' "$OUT" || fail "auto-discovery did not find ik-eve"
grep -q 'would: worktree ik-eve:app' "$OUT" || fail "discovered eve was not planned a worktree"
sed -i -e 's/^ACCOUNTS=.*/ACCOUNTS=(ada grace rex)/' -e 's/^AGENT_ACCOUNTS=.*/AGENT_ACCOUNTS=(rex)/' "$PROFILE"   # ruby leaves
sed -i '/RUBY_ONLY/d' "$SECRETS"
prov
[[ $RC -eq 0 ]] || fail "removal run exited $RC"
[[ "$(getent group ikenga-projects | cut -d: -f4)" != *ik-ruby* ]] || fail "ruby is still in the group"
[[ ! -e $SDIR/ik-ruby.env ]] || fail "ruby's secrets file survived her removal from the profile"
! getfacl -cp $PROJ/app.git | grep -q 'user:ik-ruby' || fail "ruby's ACL survived"
[[ -d "$PRINC/$(id -u ik-ruby)/home/projects/app" ]] || fail "ruby's worktree was deleted (it must be left alone)"
pass "dropping an account from the profile removes its group membership, ACL and secrets file; its worktree is left alone"

echo "==> [Container] ALL ACCOUNT TESTS PASSED"
