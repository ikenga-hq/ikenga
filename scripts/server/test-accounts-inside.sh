#!/bin/bash
# provision.sh sync-accounts: shared project mirrors + per-account clones (D-B2)
# and scoped secrets (D-B3), run as root inside an ubuntu:24.04 container
# (test-accounts-container.sh). Four accounts (ada, grace = people; rex, ruby =
# agents) plus one unmanaged ik-eve, a local bare repo as the project, a
# loopback HTTP server for the private-repo path, an "attacker" listener that
# records any request (and any token) sent to it, and a fake secrets file.
#
# Every attack below runs as an account via `as`, which is setpriv with
# --clear-groups: exactly the daemon's sessions (no supplementary groups).
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
# An attacker's listener: records the request line and whether it carried an
# Authorization header, and answers 401 so a client with a credential sends it.
cat > "$T/attacker.py" <<'PY'
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
LOG = sys.argv[1]
class H(BaseHTTPRequestHandler):
    def go(self):
        with open(LOG, 'a') as f:
            f.write('%s %s auth=%s\n' % (self.command, self.path, self.headers.get('Authorization', '-')))
        self.send_response(401)
        self.send_header('WWW-Authenticate', 'Basic realm="x"')
        self.send_header('Content-Length', '0')
        self.end_headers()
    do_GET = go
    do_POST = go
    def log_message(self, *a):
        pass
HTTPServer(('127.0.0.1', 8098), H).serve_forever()
PY
ATTLOG="$T/attacker.log"; : > "$ATTLOG"
python3 "$T/attacker.py" "$ATTLOG" >/dev/null 2>&1 &
ATT_PID=$!
trap 'kill "$HTTP_PID" "$ATT_PID" 2>/dev/null || true; rm -rf "$T"' EXIT
for _ in $(seq 1 50); do (exec 3<>/dev/tcp/127.0.0.1/8099) 2>/dev/null && (exec 3<>/dev/tcp/127.0.0.1/8098) 2>/dev/null && break; sleep 0.1; done
: > "$ATTLOG"
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
grep -q 'would: clone ik-rex:app' "$OUT" || fail "dry run did not plan rex's clone"
grep -q 'would: secrets ik-rex: +AGENT_KEY +REX_ONLY +SHARED_NOTE' "$OUT" || fail "dry run did not plan rex's secrets by name"
! getent group ikenga-projects >/dev/null || fail "dry run created a group"
[[ ! -e $PROJ && ! -e $SDIR ]] || fail "dry run created directories"
grep -q 'FAKE-' "$OUT" && fail "dry run printed a secret value"
pass "dry run plans mirrors, per-account clones and secret names, and changes nothing"

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

# ------------------------------------------- 3. the mirrors: root-owned, read-only

[[ "$(stat -c '%U %G %a' $PROJ)" == "root root 755" ]] || fail "projects dir is $(stat -c '%U %G %a' $PROJ)"
! getent group ikenga-projects >/dev/null || fail "a shared group was created (the group-writable model is gone)"
for p in app private; do
  M=$PROJ/$p.git
  [[ -d $M ]] || fail "no mirror $p"
  [[ -z "$(find $M \( ! -user root -o ! -group root -o -type l -o -perm /6027 \) -print -quit)" ]] || fail "$p: something in the mirror is not root-owned, or is group/other-writable, setgid or a symlink: $(find $M \( ! -user root -o ! -group root -o -type l -o -perm /6027 \) -print -quit)"
  ! getfacl -R -cp $M 2>/dev/null | grep -Eq '^(default:)?user:[^:]+:.*w' || fail "$p: a named-user ACL grants write"
  ! getfacl -R -cp $M 2>/dev/null | grep -q '^default:' || fail "$p: default ACLs present"
  [[ ! -e $M/hooks ]] || fail "$p: the mirror has a hooks directory"
  [[ ! -e $M/objects/info/alternates ]] || fail "$p: the mirror has alternates"
  [[ "$(git config --file $M/config --get-regexp '.' | cut -d' ' -f1 | sort | tr '\n' ' ')" == "core.bare core.filemode core.repositoryformatversion gc.auto ikenga.mirror " ]] \
    || fail "$p: mirror config has keys beyond the canonical ones: $(git config --file $M/config --list | cut -d= -f1 | tr '\n' ' ')"
  [[ "$(git config --file $M/config gc.auto)" == 0 ]] || fail "$p: gc.auto is not 0"
  for a in ada grace rex ruby; do
    getfacl -cp $M | grep -q "^user:ik-$a:r-x" || fail "$p: ik-$a has no read-only ACL entry"
  done
  ! getfacl -cp $M | grep -q '^user:ik-eve' || fail "$p: unmanaged ik-eve has an ACL"
done
[[ "$(git -C $PROJ/private.git rev-parse refs/heads/main)" == "$(git -C /fixtures/work rev-parse HEAD)" ]] || fail "private mirror did not fetch main"
[[ "$(git --git-dir $PROJ/app.git symbolic-ref HEAD)" == refs/heads/main ]] || fail "app mirror HEAD is not main"
! git config --file $PROJ/private.git/config --get-regexp 'remote|url' | grep -q . || fail "the private mirror's config names a remote or url"
! grep -rqa 'FAKE-deploy-token' $PROJ || fail "the deploy token was written into a mirror"
! grep -rqa 'FAKE-deploy-token' /etc/gitconfig 2>/dev/null || fail "the deploy token is in /etc/gitconfig"
grep -q 'directory = /srv/ikenga/projects/app.git' /etc/gitconfig || fail "safe.directory for app missing"
! grep -Eq 'directory *= *\*' /etc/gitconfig || fail "safe.directory is a wildcard"
# Read access: members only (PROJECTS_READ=members is the default).
as ada git --git-dir $PROJ/app.git rev-parse refs/heads/main >/dev/null || fail "ada cannot read the mirror with no supplementary groups"
! as eve cat $PROJ/app.git/HEAD >/dev/null 2>&1 || fail "unmanaged ik-eve can read a members-only mirror"
! as ops cat $PROJ/app.git/HEAD >/dev/null 2>&1 || fail "an unrelated user can read a members-only mirror"
pass "root:root 0755 folder, no group; mirrors root-owned, no group/other write, no hooks, canonical config, read-only member ACLs; others cannot read"

# ----------------------------------------------- 4. each account's own clone

M_APP=$PROJ/app.git
for a in ada grace rex ruby; do
  u="$(id -u ik-$a)"
  for p in app private; do
    wt="$PRINC/$u/home/projects/$p"
    [[ -f "$wt/README.md" ]] || fail "ik-$a: no checkout of $p"
    [[ "$(stat -c %u "$wt")" == "$u" && "$(stat -c %u "$wt/.git")" == "$u" && "$(stat -c %u "$wt/.git/config")" == "$u" ]] || fail "ik-$a: clone $p is not owned by the account"
    [[ -d "$wt/.git" && ! -f "$wt/.git" ]] || fail "ik-$a:$p is a linked worktree, not a clone"
    [[ "$(as "$a" git -C "$wt" branch --show-current)" == "$a/main" ]] || fail "ik-$a:$p is on '$(as "$a" git -C "$wt" branch --show-current)'"
    [[ "$(as "$a" git -C "$wt" config remote.origin.url)" == "$PROJ/$p.git" ]] || fail "ik-$a:$p origin is not the mirror"
    [[ "$(cat "$wt/.git/objects/info/alternates")" == "$PROJ/$p.git/objects" ]] || fail "ik-$a:$p does not borrow the mirror's objects"
    [[ "$(as "$a" git -C "$wt" count-objects -v | awk '/^(count|in-pack):/{n+=$2} END{print n+0}')" -le 5 ]] || fail "ik-$a:$p stored its own copy of the objects: $(as "$a" git -C "$wt" count-objects -v | tr '\n' ' ')"
  done
done
[[ ! -e "$(getent passwd ik-eve | cut -d: -f6)/projects" ]] || fail "unmanaged ik-eve got a clone"
pass "each of 4 accounts has its OWN clone (not a worktree) of both projects on <account>/main, origin = the mirror, objects borrowed (stored once); unmanaged ik-eve none"

# ------------------------- 5. daemon-style sessions use their clone and the mirror

for a in ada grace rex ruby; do
  wt="$PRINC/$(id -u ik-$a)/home/projects/app"
  as "$a" git -C "$wt" -c user.name="$a" -c user.email="$a@example.invalid" commit -q --allow-empty -m "work by $a" \
    || fail "ik-$a cannot commit in its own clone"
done
rexwt="$PRINC/$(id -u ik-rex)/home/projects/app"
adawt="$PRINC/$(id -u ik-ada)/home/projects/app"
! as rex git -C "$rexwt" rev-parse --verify -q refs/remotes/origin/ada/main >/dev/null && ! as rex git -C "$rexwt" branch -a | grep -q 'ada/main' || fail "rex's clone can see ada's branch (clones are not isolated)"
as rex git -C "$rexwt" fetch -q origin || fail "rex cannot fetch from the mirror with no supplementary groups"
pass "all accounts commit in their own clone and fetch from the mirror read-only (no supplementary groups); branches are private to each clone"

# Negative control: the mirror is readable through the ACL. Remove one
# member's entry, show the fetch fail, converge, show it work.
setfacl -R -x u:ik-grace $M_APP
if as grace git -C "$PRINC/$(id -u ik-grace)/home/projects/app" fetch -q origin 2>/dev/null; then
  fail "negative control: fetch worked without the ACL (the test proves nothing)"
fi
prov
[[ $RC -eq 0 ]] || fail "converge after ACL loss exited $RC"
grep -q "mirror $M_APP locked down" "$OUT" || fail "the lost ACL was not repaired by the run"
as grace git -C "$PRINC/$(id -u ik-grace)/home/projects/app" fetch -q origin || fail "fetch still fails after the ACL was restored"
pass "negative control: without its read ACL an account cannot read the mirror (no supplementary groups); a rerun restores it"

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
[[ "$(git -C $PROJ/app.git rev-parse refs/heads/main)" == "$UPSTREAM" ]] || fail "mirror did not fetch"
[[ "$(as rex git -C $rexwt rev-parse HEAD)" == "$HEAD1" ]] || fail "a fetch moved rex's clone"
as rex git -C $rexwt fetch -q origin && [[ "$(as rex git -C $rexwt rev-parse refs/remotes/origin/main)" == "$UPSTREAM" ]] || fail "rex cannot pull the new commit from the mirror itself"
prov; grep -q 'no changes' "$OUT" || fail "second run after the fetch reported changes"
pass "an upstream commit is fetched into the mirror only; clones stay where they are until the account fetches; next run is clean"

# ---------------------------------------------------------------------------
# 6. REGRESSION: the five attacks that blocked the group-writable design.
#    Each runs as an account with no supplementary groups and must now have
#    no effect. Every refusal is checked for the right reason by also trying
#    the plain file operation (not only a git command).
# ---------------------------------------------------------------------------

ADA_U="$(id -u ik-ada)"; REX_U="$(id -u ik-rex)"
ADA_HOME="$PRINC/$ADA_U/home"; REX_HOME="$PRINC/$REX_U/home"
echo "precious" > /root/canary; chmod 0600 /root/canary
root_tmp() { { find /tmp /var/tmp -mindepth 1 -maxdepth 1 -user root 2>/dev/null || true; } | { grep -v "^$T\$" || true; } | sort | tr '\n' ' '; }
MARK=/tmp/ik-attack-marker
rm -f $MARK.*

# ---- attack 1: root code exec through the shared repo's config/hooks
M=$PROJ/app.git
! as ada sh -c "mkdir $M/hooks" 2>/dev/null || fail "A1: ada created hooks/ in the mirror"
! as ada sh -c "printf '#!/bin/sh\nid -u > $MARK.hook\n' > $M/hooks/reference-transaction" 2>/dev/null || fail "A1: ada planted a hook in the mirror"
! as ada sh -c "echo '[remote \"origin\"]' >> $M/config" 2>/dev/null || fail "A1: ada appended to the mirror config"
! as ada git config --file $M/config remote.origin.uploadpack "sh -c 'id -u > $MARK.uploadpack'" 2>/dev/null || fail "A1: ada set uploadpack in the mirror config"
! as ada git config --file $M/config core.fsmonitor "id -u > $MARK.fsm" 2>/dev/null || fail "A1: ada set fsmonitor in the mirror config"
! as ada sh -c "touch $M/planted $M/objects/planted $M/refs/heads/planted" 2>/dev/null || fail "A1: ada wrote a file into the mirror"
! as ada sh -c "touch $PROJ/planted" 2>/dev/null || fail "A1: ada wrote into the projects folder"
# Hooks and config planted in the account's OWN clone (she can: it is hers).
for h in reference-transaction post-checkout post-merge post-commit pre-auto-gc post-rewrite pre-push; do
  as ada sh -c "printf '#!/bin/sh\nid -u >> $MARK.ownhook\n' > $adawt/.git/hooks/$h && chmod +x $adawt/.git/hooks/$h" || fail "A1: could not plant a hook in ada's own clone (test setup)"
done
as ada git -C $adawt config core.fsmonitor "sh -c 'id -u >> $MARK.ownfsm; echo 0'" || true
as ada git -C $adawt config core.hooksPath "$adawt/.git/hooks"
as ada git -C $adawt config remote.origin.uploadpack "sh -c 'id -u >> $MARK.ownup'"
# Control: the planted hook IS live for ada herself.
as ada git -C $adawt -c user.name=a -c user.email=a@example.invalid commit -q --allow-empty -m "control" 2>/dev/null || true
[[ -s $MARK.ownhook && "$(sort -u $MARK.ownhook)" == "$ADA_U" ]] || fail "A1 control: the planted hook in ada's own clone does not run for ada (the test proves nothing)"
rm -f $MARK.*
echo again >> /fixtures/work/README.md
git -C /fixtures/work commit -q -am "third" && git -C /fixtures/work push -q /fixtures/app.git main
UPSTREAM="$(git -C /fixtures/work rev-parse HEAD)"
prov
[[ $RC -eq 0 ]] || fail "A1: sync-accounts exited $RC"
[[ "$(git -C $M rev-parse refs/heads/main)" == "$UPSTREAM" ]] || fail "A1: the mirror did not fetch (the run proves nothing)"
ls $MARK.* >/dev/null 2>&1 && fail "A1: code planted by ada ran during sync-accounts: $(cat $MARK.* | tr '\n' ' ')"
[[ ! -e $M/hooks ]] || fail "A1: the mirror has hooks/"
as ada git -C $adawt config --unset remote.origin.uploadpack     # ada's own booby traps off again,
as ada git -C $adawt config --unset core.fsmonitor               # so later failures are about permissions
rm -f $adawt/.git/hooks/{reference-transaction,post-checkout,post-merge,post-commit,pre-auto-gc,post-rewrite,pre-push}
pass "A1: ada cannot plant a hook or config in the mirror; hooks/config/fsmonitor/uploadpack planted in her OWN clone never run as root during sync-accounts (control: they do run for her)"

# ---- attack 2: deploy token theft through the remote URL
P=$PROJ/private.git
! as ada git config --file $P/config remote.origin.url http://127.0.0.1:8098/x.git 2>/dev/null || fail "A2: ada changed the mirror's remote URL"
! as ada git config --file $P/config 'url.http://127.0.0.1:8098/.insteadOf' http://127.0.0.1:8099/ 2>/dev/null || fail "A2: ada set url.insteadOf in the mirror"
! as ada git config --file $P/config http.proxy http://127.0.0.1:8098 2>/dev/null || fail "A2: ada set http.proxy in the mirror"
! as ada sh -c "echo x >> $P/config" 2>/dev/null || fail "A2: ada can write the mirror config"
# A stale config that DIFFERS from the profile (what the old layout let a
# member leave behind), planted by root here: the run must ignore every key.
git config --file $P/config remote.origin.url http://127.0.0.1:8098/steal.git
git config --file $P/config 'url.http://127.0.0.1:8098/.insteadOf' http://127.0.0.1:8099/
git config --file $P/config http.proxy http://127.0.0.1:8098
git config --file $P/config remote.origin.uploadpack "sh -c 'id -u > $MARK.uploadpack'"
git config --file $P/config core.fsmonitor "sh -c 'id -u > $MARK.fsm; echo 0'"
git config --file $P/config core.hooksPath "$P/evilhooks"
git config --file $P/config core.sshCommand "sh -c 'id -u > $MARK.ssh'"
mkdir -p $P/hooks $P/evilhooks
for h in reference-transaction post-update; do printf '#!/bin/sh\nid -u > %s.hook\n' "$MARK" > $P/hooks/$h; cp $P/hooks/$h $P/evilhooks/$h; chmod +x $P/hooks/$h $P/evilhooks/$h; done
git -C /fixtures/work commit -q --allow-empty -m "private moves" && git -C /fixtures/work push -q /fixtures/http/private.git main
UPSTREAM2="$(git -C /fixtures/work rev-parse HEAD)"
: > "$ATTLOG"; rm -f $MARK.*
prov
[[ $RC -eq 0 ]] || fail "A2: sync-accounts exited $RC"
[[ "$(git -C $P rev-parse refs/heads/main)" == "$UPSTREAM2" ]] || fail "A2: root's fetch did not use the profile URL (private mirror did not advance)"
[[ ! -s "$ATTLOG" ]] || fail "A2: the attacker's listener received a request: $(cut -c1-60 "$ATTLOG" | head -3)"
! grep -q 'FAKE-deploy-token\|Basic' "$ATTLOG" || fail "A2: the deploy token reached the attacker"
ls $MARK.* >/dev/null 2>&1 && fail "A2: planted stale config/hooks ran as root: $(cat $MARK.* | tr '\n' ' ')"
[[ -z "$(git config --file $P/config --get-regexp 'remote|url|http|core.fsmonitor|core.hookspath|core.sshcommand')" ]] || fail "A2: the stale config keys survived"
[[ ! -e $P/hooks && ! -e $P/evilhooks/x ]] || fail "A2: planted hooks/ survived"
grep -q 'config reset to the canonical one' "$OUT" || fail "A2: the run did not report resetting the stale config"
pass "A2: nobody can change the mirror's URL/insteadOf/proxy; with a stale differing config planted (as the old layout allowed), root's fetch used the profile URL, the attacker's listener got nothing, no token left the box, the config was reset"

# ---- attack 3: root writes through a member's symlink
M=$PROJ/app.git
! as ada ln -s /root/canary $M/.fetch.err 2>/dev/null || fail "A3: ada made a symlink in the mirror"
! as ada ln -s /root/canary $PROJ/.fetch.err 2>/dev/null || fail "A3: ada made a symlink in the projects folder"
! as ada ln -s /root/canary "$PROJ/.new-app.AAAAAA" 2>/dev/null || fail "A3: ada made a symlink named like the provisioner's temp dir"
# A legacy layout (symlink + alternates + loose perms), planted by root.
ln -s /root/canary $M/.fetch.err
mkdir -p $M/objects/info; echo /tmp/evil-objects > $M/objects/info/alternates
chmod -R g+w $M; chmod g+s $M
# And every directory ada CAN write: her home, ~/projects, her clone.
as ada ln -s /root/canary "$ADA_HOME/.fetch.err"
as ada ln -s /root/canary "$ADA_HOME/projects/.fetch.err"
as ada ln -s /root/canary "$ADA_HOME/projects/app.git"
as ada ln -s /root/canary "$ADA_HOME/projects/app.fetch.err"
as ada ln -s /root/canary "$adawt/.fetch.err"
BEFORE_TMP="$(root_tmp)"
echo yet-again >> /fixtures/work/README.md
git -C /fixtures/work commit -q -am "fourth" && git -C /fixtures/work push -q /fixtures/app.git main
prov
[[ $RC -eq 0 ]] || fail "A3: sync-accounts exited $RC"
[[ "$(cat /root/canary)" == precious ]] || fail "A3: root's file was truncated or changed through a symlink"
[[ -z "$(find "$PRINC"/*/home -user root -print -quit)" ]] || fail "A3: root wrote into a member-owned directory: $(find "$PRINC"/*/home -user root | head -3)"
[[ "$(root_tmp)" == "$BEFORE_TMP" ]] || fail "A3: the run left root-owned files in /tmp: $(root_tmp)"
[[ ! -e $M/.fetch.err && ! -L $M/.fetch.err ]] || fail "A3: the planted symlink was left in the mirror"
[[ ! -e $M/objects/info/alternates ]] || fail "A3: the planted alternates survived"
[[ -z "$(find $M \( ! -user root -o -type l -o -perm /6027 \) -print -quit)" ]] || fail "A3: the legacy loose perms were not repaired: $(find $M \( ! -user root -o -type l -o -perm /6027 \) -exec stat -c '%n %U %a' {} + | head -3)"
pass "A3: no member can create a symlink where root writes; with symlinks in every dir a member CAN write plus a legacy layout planted, the root-only canary is untouched, root wrote nothing into any account's home or /tmp, and the legacy layout was repaired"

# ---- attack 4: account A runs code as / changes account B
! as ada sh -c "ls $REX_HOME" >/dev/null 2>&1 || fail "A4: ada can list rex's home"
! as ada sh -c "echo '[core]' >> $rexwt/.git/config" 2>/dev/null || fail "A4: ada wrote rex's clone config"
! as ada sh -c "printf '#!/bin/sh\nid -u > $MARK.crosshook\n' > $rexwt/.git/hooks/post-commit" 2>/dev/null || fail "A4: ada planted a hook in rex's clone"
! as ada git -C $rexwt config core.fsmonitor "id -u > $MARK.crossfsm" 2>/dev/null || fail "A4: ada set fsmonitor in rex's clone"
! as ada cat $rexwt/.git/index >/dev/null 2>&1 || fail "A4: ada read rex's index"
# Ada's hooks/config are ada's alone: rex's commit in rex's own clone, and a
# commit/checkout/status in the mirror's neighbourhood, never run them.
as ada sh -c "printf '#!/bin/sh\nid -u >> $MARK.crosshook\n' > $adawt/.git/hooks/post-commit && chmod +x $adawt/.git/hooks/post-commit"
as ada git -C $adawt config core.fsmonitor "sh -c 'id -u >> $MARK.crossfsm; echo 0'"
as rex git -C $rexwt -c user.name=r -c user.email=r@example.invalid commit -q --allow-empty -m "rex after ada's hooks" || fail "A4: rex's commit failed"
as rex git -C $rexwt status --short >/dev/null || fail "A4: rex's status failed"
as rex git -C $rexwt fetch -q origin
ls $MARK.crosshook $MARK.crossfsm >/dev/null 2>&1 && fail "A4: ada's hook/fsmonitor ran for rex: $(cat $MARK.cross* | tr '\n' ' ')"
# Control: they do run for ada.
as ada git -C $adawt -c user.name=a -c user.email=a@example.invalid commit -q --allow-empty -m "ada commits"
[[ -s $MARK.crosshook && "$(sort -u $MARK.crosshook)" == "$ADA_U" ]] || fail "A4 control: ada's own hook did not run for ada"
rm -f $MARK.*
[[ "$(stat -c %a "$REX_HOME")" == 700 ]] || fail "A4: rex's home is not 0700"
pass "A4: ada cannot read or write rex's clone (hooks, config, index); a hook and fsmonitor in ada's clone run for ada only, never for rex (control: they do run for ada)"

# ---- attack 5: rewrite refs, delete refs, corrupt objects, read other clones
M=$PROJ/app.git
MAIN_BEFORE="$(git -C $M rev-parse refs/heads/main)"; REFS_BEFORE="$(git -C $M for-each-ref | sha256sum)"
OBJ_BEFORE="$(find $M/objects -type f | sort | sha256sum)"
EVIL="$(git -C $M rev-parse refs/heads/main~1)"
! as ada sh -c "echo $EVIL > $M/refs/heads/main" 2>/dev/null || fail "A5: ada wrote a ref file in the mirror"
! as ada git --git-dir $M update-ref refs/heads/rex/main "$EVIL" 2>/dev/null || fail "A5: ada created refs/heads/rex/main in the mirror"
! as ada git --git-dir $M update-ref -d refs/heads/main 2>/dev/null || fail "A5: ada deleted a shared ref"
! as ada git --git-dir $M update-ref refs/heads/main "$EVIL" 2>/dev/null || fail "A5: ada rewrote the shared main"
! as ada sh -c "echo junk | git --git-dir $M hash-object -w --stdin" >/dev/null 2>&1 || fail "A5: ada wrote an object into the mirror"
! as ada sh -c "touch $M/objects/aa; mkdir $M/objects/ab; rm -rf $M/objects/pack" 2>/dev/null || fail "A5: ada altered the mirror's object store"
! as ada git -C $adawt push -q origin HEAD:refs/heads/evil 2>/dev/null || fail "A5: ada pushed to the mirror"
! as ada git -C $adawt push -q origin --delete main 2>/dev/null || fail "A5: ada deleted main through the mirror"
! as ada git -C $rexwt update-ref refs/heads/rex/main "$EVIL" 2>/dev/null || fail "A5: ada rewrote rex's branch in rex's clone"
! as ada git -C $rexwt update-ref -d refs/heads/rex/main 2>/dev/null || fail "A5: ada deleted rex's branch"
! as ada sh -c "ls $rexwt/.git/objects" >/dev/null 2>&1 || fail "A5: ada listed rex's object store"
[[ "$(git -C $M rev-parse refs/heads/main)" == "$MAIN_BEFORE" && "$(git -C $M for-each-ref | sha256sum)" == "$REFS_BEFORE" ]] || fail "A5: the mirror's refs changed"
[[ "$(find $M/objects -type f | sort | sha256sum)" == "$OBJ_BEFORE" ]] || fail "A5: the mirror's object store changed"
git -C $M fsck --no-dangling >/dev/null 2>&1 || fail "A5: the mirror no longer passes fsck"
[[ "$(as rex git -C $rexwt rev-parse refs/heads/rex/main)" == "$(as rex git -C $rexwt rev-parse HEAD)" ]] || fail "A5: rex's branch moved"
prov; [[ $RC -eq 0 ]] || fail "A5: run after the attacks exited $RC"
pass "A5: ada cannot update, create or delete refs, write or corrupt objects, or push to the mirror or into rex's clone, nor list rex's objects; refs, objects and fsck are unchanged"

# --- the mirror is still usable by everyone after all of that
for a in ada grace rex ruby; do as "$a" git -C "$PRINC/$(id -u ik-$a)/home/projects/app" fetch -q origin || fail "ik-$a can no longer fetch from the mirror"; done
rm -f /root/canary
pass "after the five attacks every account still fetches from the mirror"

# --------------------------------------------- read access: world vs members

sed -i '/^PROJECTS_READ=/d' "$PROFILE"; echo 'PROJECTS_READ=world' >> "$PROFILE"
prov
[[ $RC -eq 0 ]] || fail "PROJECTS_READ=world run exited $RC"
as eve git --git-dir $PROJ/app.git rev-parse refs/heads/main >/dev/null || fail "world mode: unmanaged ik-eve cannot read the mirror"
[[ -z "$(getfacl -cp $PROJ/app.git | grep '^user:ik-')" ]] || fail "world mode left per-member ACL entries"
[[ -z "$(find $PROJ/app.git \( ! -user root -o -perm /6022 -o ! -perm -004 \) -print -quit)" ]] || fail "world mode: modes are wrong"
! as eve sh -c "touch $PROJ/app.git/x" 2>/dev/null || fail "world mode: other can write"
sed -i '/^PROJECTS_READ=/d' "$PROFILE"
prov
[[ $RC -eq 0 ]] || fail "back to members run exited $RC"
! as eve cat $PROJ/app.git/HEAD >/dev/null 2>&1 || fail "members mode did not close the mirror to ik-eve again"
as ada git --git-dir $PROJ/app.git rev-parse refs/heads/main >/dev/null || fail "members mode: ada lost access"
pass "PROJECTS_READ=world opens the mirrors read-only to every user and drops the ACLs; switching back to members closes them again"

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
grep -q 'would: clone ik-eve:app' "$OUT" || fail "discovered eve was not planned a clone"
sed -i -e 's/^ACCOUNTS=.*/ACCOUNTS=(ada grace rex)/' -e 's/^AGENT_ACCOUNTS=.*/AGENT_ACCOUNTS=(rex)/' "$PROFILE"   # ruby leaves
sed -i '/RUBY_ONLY/d' "$SECRETS"
prov
[[ $RC -eq 0 ]] || fail "removal run exited $RC"
[[ ! -e $SDIR/ik-ruby.env ]] || fail "ruby's secrets file survived her removal from the profile"
! getfacl -R -cp $PROJ/app.git | grep -q 'user:ik-ruby' || fail "ruby's ACL survived"
! as ruby cat $PROJ/app.git/HEAD >/dev/null 2>&1 || fail "ruby can still read the members-only mirror"
[[ -d "$PRINC/$(id -u ik-ruby)/home/projects/app/.git" ]] || fail "ruby's clone was deleted (it must be left alone)"
pass "dropping an account from the profile removes its read ACL and secrets file; its own clone is left alone"

# A project removed from the profile loses its safe.directory entry; the
# mirror and the clones stay on disk. Change a managed block repeatedly: only
# the last five backups of /etc/gitconfig are kept.
for i in 1 2 3 4 5 6 7 8; do : > /etc/gitconfig.bak-2020010100000$i; done
sed -i 's/^PROJECTS=.*/PROJECTS=("app=file:\/\/\/fixtures\/app.git")/' "$PROFILE"
prov
[[ $RC -eq 0 ]] || fail "project removal run exited $RC"
! grep -q 'private.git' /etc/gitconfig || fail "a removed project's safe.directory entry survived"
grep -q 'app.git' /etc/gitconfig || fail "the remaining project's safe.directory entry vanished"
[[ -d $PROJ/private.git && -d "$PRINC/$(id -u ik-ada)/home/projects/private/.git" ]] || fail "removing a project deleted data"
[[ "$(ls /etc/gitconfig.bak-* | wc -l)" -le 5 ]] || fail "gitconfig backups were not pruned: $(ls /etc/gitconfig.bak-* | wc -l)"
ls /etc/gitconfig.bak-2020* >/dev/null 2>&1 && [[ ! -e /etc/gitconfig.bak-20200101000001 ]] || fail "the oldest backups were kept instead of the newest"
pass "a removed project loses its safe.directory entry (data stays); /etc/gitconfig backups are pruned to the last five"

echo "==> [Container] ALL ACCOUNT TESTS PASSED"
