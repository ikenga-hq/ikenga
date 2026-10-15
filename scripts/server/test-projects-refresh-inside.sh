#!/bin/bash
# provision.sh refresh-projects: refresh the shared mirrors and fast-forward the
# existing clones of PROJECTS_FASTFORWARD accounts, plus the
# ikenga-projects-refresh timer. Run as root inside an ubuntu:24.04 container
# (test-projects-refresh-container.sh), with real git and real accounts.
#
# Accounts: ada = a person (NOT listed), rex and ruby = agents (listed), eve =
# a person who is not even managed. Nine projects, each a local bare repo; rex's
# clones are put in the states the script must handle (clean, dirty, untracked,
# diverged, detached, planted hooks, a hanging filter); ruby's are used for the
# path attacks (a symlinked clone, a clone owned by someone else).
#
# Every attack runs as the account via `as`: setpriv --clear-groups, exactly the
# daemon's sessions.
set -euo pipefail

T="$(mktemp -d)"
PROV=/root/prov
PROVISION="/work/provision.sh"
PROFILE="$PROV/profile.env"
OUT="$T/out"
PROJ=/srv/ikenga/projects
PRINC=/var/lib/ikenga-test/principals
MARK=/var/tmp/markers
SD="$T/systemd"
STABLE="$T/sbin/ikenga-provision"
INSTALL="$T/opt-ikenga"
SYSLOG="$T/systemctl.log"
GITLOG=/var/log/git-shim.log
NAMES=(clean dirty untracked diverged detached hooks slow symlink foreign)

fail() { echo "FAIL: $*" >&2; [[ -f "$OUT" ]] && { echo "--- last output ---" >&2; cat "$OUT" >&2; }; exit 1; }
pass() { echo "==> [Container] $* PASSED"; }
trap 'rm -rf "$T"' EXIT

# ------------------------------------------------------------------ host

mkacct() {   # name uid : mimics what `ikenga-server accounts create` leaves behind
  groupadd -g "$2" "ik-$1"
  useradd -u "$2" -g "$2" -M -d "$PRINC/$2/home" -s /bin/sh "ik-$1"
  install -d -m 0700 -o "$2" -g "$2" "$PRINC/$2/home"
}
mkdir -p "$PRINC" "$MARK" "$SD" "$T/sbin" "$T/bin" "$T/state" "$INSTALL"
chmod 1777 "$MARK"
mkacct ada 20001; mkacct rex 20003; mkacct ruby 20004; mkacct eve 20005
home_of() { echo "$PRINC/$(id -u "ik-$1")/home"; }
wt_of() { echo "$(home_of "$1")/projects/$2"; }

as() {
  local n="$1" u; shift
  u="$(id -u "ik-$n")"
  ( cd / && setpriv --reuid="$u" --regid="$u" --clear-groups \
      env -i HOME="$PRINC/$u/home" PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 \
      GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.invalid GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.invalid "$@" )
}

# A stateful systemctl stand-in: the container has no systemd. It records every
# call so the test can see what the provisioner asked for.
cat > "$T/bin/systemctl" <<EOF
#!/bin/bash
echo "\$*" >> "$SYSLOG"
case "\$1" in
  enable) [[ "\$2" == --now ]] && touch "$T/state/\$3"; exit 0 ;;
  disable) [[ "\$2" == --now ]] && rm -f "$T/state/\$3"; exit 0 ;;
  is-enabled|is-active) [[ "\$2" == --quiet ]] && [[ -e "$T/state/\$3" ]]; exit \$? ;;
  *) exit 0 ;;
esac
EOF
chmod +x "$T/bin/systemctl"; : > "$SYSLOG"

# ------------------------------------------------------------- fixtures

git config --global user.name test; git config --global user.email t@example.invalid
git config --global init.defaultBranch main
mkdir -p /fixtures-work; git init -q -b main /fixtures-work
W=/fixtures-work
echo hello > $W/README.md
echo one > $W/a.txt
printf 'a.txt filter=slow\n' > $W/.gitattributes
git -C $W add -A && git -C $W commit -q -m first
mkdir -p /fixtures
for n in "${NAMES[@]}"; do git clone -q --bare $W "/fixtures/$n.git"; done
advance() {   # a second commit on every upstream: changes a.txt, adds b.txt (README is untouched)
  echo "two $1" > $W/a.txt; echo b > $W/b-$1.txt
  git -C $W add -A && git -C $W commit -q -m "second $1"
  for n in "${NAMES[@]}"; do git -C $W push -q "/fixtures/$n.git" main; done
}

install -d -m 0700 "$PROV" "$INSTALL"
{
  echo 'ACCOUNTS=(ada rex ruby)'
  echo 'AGENT_ACCOUNTS=(rex ruby)'
  printf 'PROJECTS=('; for n in "${NAMES[@]}"; do printf '"%s=file:///fixtures/%s.git" ' "$n" "$n"; done; echo ')'
  echo 'PROJECTS_FASTFORWARD=(ruby rex)'
} > "$PROFILE"
chmod 0600 "$PROFILE"

export IKENGA_SYSTEMD_DIR="$SD" IKENGA_PROVISION_STABLE="$STABLE" IKENGA_PROJECTS_LOCK="$T/projects.lock" INSTALL_DIR="$INSTALL"
export IKENGA_REFRESH_GIT_TIMEOUT=4
export PATH="$T/bin:$PATH"
RC=0
prov() { RC=0; "$PROVISION" sync-accounts --profile "$PROFILE" "$@" >"$OUT" 2>&1 || RC=$?; }
refr() { RC=0; "$PROVISION" refresh-projects --profile "$PROFILE" "$@" >"$OUT" 2>&1 || RC=$?; }
profile_set() {   # profile_set VAR 'line' : replace or append one assignment
  sed -i "/^$1=/d" "$PROFILE"; echo "$2" >> "$PROFILE"
}

# ---------------------------------------- 1. sync-accounts plans the timer

prov --dry-run
[[ $RC -eq 0 ]] || fail "sync-accounts dry run exited $RC"
grep -q 'write .*ikenga-projects-refresh.timer' "$OUT" || fail "dry run did not plan the timer unit"
grep -q 'OnCalendar=daily' "$OUT" || fail "dry run did not show the default schedule"
[[ ! -e $SD/ikenga-projects-refresh.timer && ! -e $SD/ikenga-projects-refresh.service && ! -e $STABLE && ! -e $PROJ ]] || fail "dry run wrote something"
[[ ! -s $SYSLOG ]] || fail "dry run called systemctl: $(cat "$SYSLOG")"
pass "sync-accounts --dry-run plans the refresh units and writes nothing"

# --------------------------------------------- 2. first run: units + clones

prov
[[ $RC -eq 0 ]] || fail "first sync-accounts exited $RC"
for n in "${NAMES[@]}"; do for a in ada rex ruby; do
  [[ -d "$(wt_of $a $n)/.git" ]] || fail "ik-$a has no clone of $n"
done; done
[[ ! -e "$(home_of eve)/projects" ]] || fail "unlisted, projects-less eve got a clone dir"
S=$SD/ikenga-projects-refresh.service; TM=$SD/ikenga-projects-refresh.timer
[[ -f $S && -f $TM ]] || fail "units not installed"
grep -q '^Type=oneshot$' $S || fail "service is not oneshot"
grep -q "^ExecStart=$STABLE refresh-projects\$" $S || fail "service ExecStart is wrong: $(grep ExecStart $S)"
grep -q '^OnCalendar=daily$' $TM || fail "timer schedule is not the default"
grep -q '^Persistent=true$' $TM || fail "timer is not persistent"
grep -q '^RandomizedDelaySec=15min$' $TM || fail "timer has no randomized delay"
[[ -x $STABLE ]] && cmp -s $STABLE "$PROVISION" || fail "stable provisioner copy missing or different"
grep -qx 'enable --now ikenga-projects-refresh.timer' "$SYSLOG" || fail "timer was not enabled: $(cat "$SYSLOG")"
grep -qx 'daemon-reload' "$SYSLOG" || fail "no daemon-reload"
pass "sync-accounts creates the clones and installs + enables ikenga-projects-refresh.{service,timer} (daily, persistent, randomized)"

prov
[[ $RC -eq 0 ]] || fail "rerun exited $RC"
grep -q 'no changes: the host already matches this profile' "$OUT" || fail "rerun reported changes: $(cat "$OUT")"
pass "a rerun changes nothing (units idempotent)"

# ---------------------------------------------- 3. the timer's schedule knob

profile_set PROJECTS_REFRESH_SCHEDULE 'PROJECTS_REFRESH_SCHEDULE="Mon *-*-* 05:30:00 UTC"'
: > "$SYSLOG"; prov
[[ $RC -eq 0 ]] || fail "schedule change exited $RC"
grep -q '^OnCalendar=Mon \*-\*-\* 05:30:00 UTC$' $TM || fail "timer did not take the configured schedule: $(grep OnCalendar $TM)"
grep -qx 'restart ikenga-projects-refresh.timer' "$SYSLOG" || fail "the changed timer was not restarted: $(cat "$SYSLOG")"

profile_set PROJECTS_REFRESH_SCHEDULE 'PROJECTS_REFRESH_SCHEDULE=off'
: > "$SYSLOG"; prov
[[ $RC -eq 0 ]] || fail "schedule off exited $RC"
[[ ! -e $S && ! -e $TM ]] || fail "'off' left the units behind"
grep -qx 'disable --now ikenga-projects-refresh.timer' "$SYSLOG" || fail "'off' did not disable the timer: $(cat "$SYSLOG")"
grep -q 'unit ikenga-projects-refresh.timer removed' "$OUT" || fail "'off' did not report the removal"
[[ -d "$(wt_of rex clean)/.git" && -d $PROJ/clean.git ]] || fail "'off' touched the clones or mirrors"

profile_set PROJECTS_REFRESH_SCHEDULE 'PROJECTS_REFRESH_SCHEDULE="*-*-* 03,15:00:00"'
prov
[[ $RC -eq 0 && -f $TM ]] && grep -q '^OnCalendar=\*-\*-\* 03,15:00:00$' $TM || fail "re-enabling did not install the timer"

cp "$PROFILE" "$PROFILE.keep"
for bad in 'daily; touch /var/tmp/markers/pwn' '$(touch /var/tmp/markers/pwn)' 'daily
ExecStartPre=/bin/true' '`id`' ''; do
  profile_set PROJECTS_REFRESH_SCHEDULE "PROJECTS_REFRESH_SCHEDULE='$bad'"
  prov
  [[ $RC -ne 0 ]] || fail "schedule '$bad' was accepted"
  [[ ! -e $MARK/pwn ]] || fail "schedule '$bad' ran a command"
done
cp "$PROFILE.keep" "$PROFILE"
grep -q '^OnCalendar=\*-\*-\* 03,15:00:00$' $TM || fail "a rejected schedule changed the timer"
profile_set PROJECTS_REFRESH_SCHEDULE 'PROJECTS_REFRESH_SCHEDULE=daily'
prov; [[ $RC -eq 0 ]] || fail "back to daily exited $RC"

# No PROJECTS at all: the units go away.
cp "$PROFILE" "$PROFILE.keep"
printf 'ACCOUNTS=(ada rex ruby)\nAGENT_ACCOUNTS=(rex ruby)\n' > "$PROFILE"
prov
[[ $RC -eq 0 && ! -e $S && ! -e $TM ]] || fail "an empty PROJECTS kept the units (rc=$RC)"
cp "$PROFILE.keep" "$PROFILE"
prov; [[ $RC -eq 0 && -f $TM ]] || fail "units did not come back with PROJECTS"
pass "schedule is configurable and validated (injection refused), 'off' disables and removes the units, empty PROJECTS removes them, they come back"

# --------------------------------------------- 4. profile validation

cp "$PROFILE" "$PROFILE.keep"
profile_set PROJECTS_FASTFORWARD 'PROJECTS_FASTFORWARD=(rex mallory)'
refr --dry-run; [[ $RC -ne 0 ]] && grep -q "PROJECTS_FASTFORWARD names 'mallory'" "$OUT" || fail "an unmanaged fast-forward account was accepted (rc=$RC)"
profile_set PROJECTS_FASTFORWARD 'PROJECTS_FASTFORWARD=("rex;id")'
refr --dry-run; [[ $RC -ne 0 ]] && grep -q "PROJECTS_FASTFORWARD entry" "$OUT" || fail "a malformed fast-forward name was accepted (rc=$RC)"
profile_set PROJECTS_FASTFORWARD 'PROJECTS_FASTFORWARD=(root)'
refr --dry-run; [[ $RC -ne 0 ]] || fail "'root' was accepted as a fast-forward account"
cp "$PROFILE.keep" "$PROFILE"
pass "PROJECTS_FASTFORWARD must name managed accounts with plain names"

# ----------------------------- 5. controls: the attacks below do fire when unguarded

# Hooks and fsmonitor really run for a plain git as the account (so their
# absence later means something).
rm -rf /var/tmp/ctl && mkdir -p /var/tmp/ctl && chown ik-rex /var/tmp/ctl
as rex git init -q -b main /var/tmp/ctl/up
as rex git -C /var/tmp/ctl/up commit -q --allow-empty -m one
as rex git clone -q /var/tmp/ctl/up /var/tmp/ctl/down
as rex git -C /var/tmp/ctl/up commit -q --allow-empty -m two
printf '#!/bin/sh\necho fired >> %s/ctl-post-merge\n' "$MARK" > /var/tmp/ctl/down/.git/hooks/post-merge
printf '#!/bin/sh\necho fired >> %s/ctl-fsmonitor\nexit 1\n' "$MARK" > /var/tmp/ctl/fsm; chmod 755 /var/tmp/ctl/down/.git/hooks/post-merge /var/tmp/ctl/fsm
chown ik-rex /var/tmp/ctl/down/.git/hooks/post-merge /var/tmp/ctl/fsm
as rex git -C /var/tmp/ctl/down config core.fsmonitor /var/tmp/ctl/fsm
as rex git -C /var/tmp/ctl/down pull -q --ff-only >/dev/null 2>&1 || true
[[ -s $MARK/ctl-post-merge ]] || fail "control: a planted post-merge hook did not fire for a plain git merge (the test proves nothing)"
as rex git -C /var/tmp/ctl/down status >/dev/null 2>&1 || true
[[ -s $MARK/ctl-fsmonitor ]] || fail "control: a planted core.fsmonitor did not fire for a plain git status (the test proves nothing)"
rm -rf /var/tmp/ctl; rm -f $MARK/ctl-*
pass "controls: a plain git as the account DOES run a planted post-merge hook and core.fsmonitor"

# ---------------------------------------- 6. put the clones in their states

advance 1       # upstreams move ahead; the mirrors are now stale

cfg() { local a="$1" n="$2"; shift 2; as "$a" git -C "$(wt_of "$a" "$n")" "$@"; }
commit_in() { cfg "$1" "$2" -c user.name=x -c user.email=x@example.invalid commit -q --allow-empty -m "own work"; }

echo "local edit" >> "$(wt_of rex dirty)/README.md"                  # a tracked, uncommitted change
as rex sh -c "echo scratch > '$(wt_of rex untracked)/scratch.tmp'"   # an untracked file
commit_in rex diverged                                               # a commit upstream does not have
cfg rex detached checkout -q --detach
# Planted hooks: the repo's own hooks dir, core.hooksPath at a dir the account
# owns, a fsmonitor program. All write a marker if they run.
hk="$(wt_of rex hooks)"
as rex mkdir -p /var/tmp/evilhooks
for h in post-merge post-checkout reference-transaction pre-auto-gc post-rewrite; do
  for d in "$hk/.git/hooks" /var/tmp/evilhooks; do
    printf '#!/bin/sh\necho "%s ran" >> %s/hook-%s\n' "$h" "$MARK" "$h" | as rex tee "$d/$h" >/dev/null
    as rex chmod 755 "$d/$h"
  done
done
printf '#!/bin/sh\necho ran >> %s/fsmonitor\nexit 1\n' "$MARK" | as rex tee /var/tmp/evilfsm >/dev/null; as rex chmod 755 /var/tmp/evilfsm
cfg rex hooks config core.hooksPath /var/tmp/evilhooks
cfg rex hooks config core.fsmonitor /var/tmp/evilfsm
cfg rex hooks config core.sshCommand /var/tmp/evilfsm
# A filter that never returns, on the file the fast-forward rewrites, and that
# leaves a session-detached background process holding its output open (which
# would stall a pipe-reading provisioner for its whole 250s).
cfg rex slow config filter.slow.smudge 'setsid sleep 250 & sleep 300; cat'

# Path attacks (ruby): a clone path that is a symlink to someone else's repo
# (which ruby owns and which is behind, so an unguarded run WOULD move it), and
# a clone directory owned by another user.
rm -rf /srv/victim; git clone -q --no-local "$PROJ/symlink.git" /srv/victim
chown -R ik-ruby /srv/victim
rgit() { git -c safe.directory='*' "$@"; }   # root reading an account-owned repo, for the assertions only
VICTIM_HEAD="$(rgit -C /srv/victim rev-parse HEAD)"
rm -rf "$(wt_of ruby symlink)"; ln -s /srv/victim "$(wt_of ruby symlink)"
chown -h ik-ruby "$(wt_of ruby symlink)"
chown root:root "$(wt_of ruby foreign)"
FOREIGN_HEAD="$(rgit -C "$(wt_of ruby foreign)" rev-parse HEAD)"

hook_markers() { find "$MARK" -maxdepth 1 -name 'hook-*' -printf '%f '; }
snap() {   # snap <account> <project> : HEAD, branch, status and every work-tree byte (no .git)
  local wt; wt="$(wt_of "$1" "$2")"
  { as "$1" git -C "$wt" -c core.fsmonitor=false rev-parse HEAD
    as "$1" git -C "$wt" symbolic-ref -q HEAD || echo detached
    as "$1" git -C "$wt" -c core.fsmonitor=false status --porcelain --untracked-files=all
    ( cd "$wt" && find . -path ./.git -prune -o -type f -print0 | sort -z | xargs -0 sha256sum )
  } 2>&1 | sha256sum
}
tree_of() { tar -C "$(home_of "$1")" -cf - . 2>/dev/null | sha256sum; }   # every byte of an account's home, incl. .git
UP2="$(git -C $W rev-parse HEAD)"
mirror_head() { git --git-dir "$PROJ/$1.git" rev-parse refs/heads/main; }
UP1="$(git -C $W rev-parse HEAD~1)"
[[ "$(mirror_head clean)" == "$UP1" ]] || fail "setup: the mirror is not stale"

declare -A BEFORE
for n in dirty untracked diverged detached; do BEFORE[$n]="$(snap rex $n)"; done
ADA_BEFORE="$(tree_of ada)"; ADA_HEAD="$(as ada git -C "$(wt_of ada clean)" rev-parse HEAD)"
MIRRORS_BEFORE="$(for n in "${NAMES[@]}"; do git --git-dir "$PROJ/$n.git" for-each-ref; done | sha256sum)"
RUBY_BEFORE="$(tree_of ruby)"; REX_TREE_BEFORE="$(tree_of rex)"
UNITS_BEFORE="$(ls -l --time-style=+%s "$SD" | sha256sum)"

# ---------------------------------------------- 7. --dry-run changes nothing

: > "$SYSLOG"
refr --dry-run
[[ $RC -eq 1 ]] || fail "dry run should still report the two path refusals (rc 1), got $RC"
grep -q 'is a symlink; refusing' "$OUT" || fail "dry run did not refuse the symlinked clone"
grep -q '\[dry-run\] .*fetch' "$OUT" || fail "dry run did not describe the fetch"
grep -q 'ik-rex:clean: would fetch' "$OUT" || fail "dry run did not describe rex's fast-forward"
[[ "$(for n in "${NAMES[@]}"; do git --git-dir "$PROJ/$n.git" for-each-ref; done | sha256sum)" == "$MIRRORS_BEFORE" ]] || fail "dry run changed a mirror"
[[ "$(tree_of rex)" == "$REX_TREE_BEFORE" && "$(tree_of ruby)" == "$RUBY_BEFORE" && "$(tree_of ada)" == "$ADA_BEFORE" ]] || fail "dry run changed an account's files"
[[ "$(ls -l --time-style=+%s "$SD" | sha256sum)" == "$UNITS_BEFORE" && ! -s $SYSLOG ]] || fail "dry run touched units or systemctl"
[[ ! -e $MARK/fsmonitor && -z "$(hook_markers)" ]] || fail "dry run ran a planted hook"
pass "refresh-projects --dry-run fetches nothing, merges nothing, writes no unit, and says what it would do"

# ------------------------------------------------------ 8. the real run

: > "$GITLOG"; chmod 666 "$GITLOG"
printf '#!/bin/sh\nprintf "%%s\\t%%s\\t%%s\\n" "$(id -u)" "$PWD" "$*" >> %s\nexec /usr/bin/git "$@"\n' "$GITLOG" > /usr/local/bin/git
chmod 755 /usr/local/bin/git
START=$(date +%s)
refr
ELAPSED=$(( $(date +%s) - START ))
rm -f /usr/local/bin/git
[[ $RC -eq 1 ]] || fail "refresh-projects should exit non-zero (refusals + a timeout), got $RC"
[[ $ELAPSED -lt 120 ]] || fail "the run took ${ELAPSED}s: the hanging filter was not bounded"
grep -q 'some project refresh steps failed' "$OUT" || fail "no failure summary"

# a. the mirrors have the new commits
for n in "${NAMES[@]}"; do [[ "$(mirror_head $n)" == "$UP2" ]] || fail "mirror $n was not refreshed"; done
pass "every mirror was fetched (new upstream commits arrived)"

# b. listed accounts' clean clones fast-forwarded
[[ "$(cfg rex clean rev-parse HEAD)" == "$UP2" ]] || fail "rex's clean clone did not fast-forward"
[[ "$(cfg rex clean symbolic-ref HEAD)" == refs/heads/rex/main ]] || fail "the fast-forward moved rex to another branch"
[[ -f "$(wt_of rex clean)/b-1.txt" && "$(cat "$(wt_of rex clean)/a.txt")" == "two 1" ]] || fail "the work tree was not updated"
[[ -z "$(cfg rex clean -c core.fsmonitor=false status --porcelain)" ]] || fail "the fast-forwarded clone is dirty"
[[ "$(cfg ruby clean rev-parse HEAD)" == "$UP2" ]] || fail "ruby's clean clone did not fast-forward (the run must continue past other accounts' failures)"
grep -q 'clone ik-rex:clean rex/main fast-forwarded' "$OUT" || fail "the fast-forward was not reported"
pass "clean + on a branch + strictly behind: rex's and ruby's clones fast-forwarded in place (same branch, work tree updated)"

# c. everything else is left exactly as it was
for n in dirty untracked diverged detached; do
  [[ "$(snap rex $n)" == "${BEFORE[$n]}" ]] || fail "rex's $n clone changed (HEAD/branch/status/work tree)"
done
grep -q 'ik-rex:dirty: has uncommitted' "$OUT" || fail "dirty not explained"
grep -q 'ik-rex:untracked: has uncommitted' "$OUT" || fail "untracked not explained"
grep -q 'ik-rex:diverged: .*has commits upstream does not' "$OUT" || fail "diverged not explained"
grep -q 'ik-rex:detached: HEAD is detached' "$OUT" || fail "detached not explained"
[[ "$(tail -1 "$(wt_of rex dirty)/README.md")" == "local edit" && -f "$(wt_of rex untracked)/scratch.tmp" ]] || fail "local edits were lost"
[[ "$(cfg rex diverged log -1 --format=%s)" == "own work" ]] || fail "rex's own commit is gone"
[[ "$(cfg rex detached symbolic-ref -q HEAD || echo detached)" == detached ]] || fail "the detached HEAD was re-attached"
pass "dirty (tracked), dirty (untracked), diverged and detached clones are byte-identical in HEAD, branch, status and work tree"

# d. an account that is not listed is not touched at all
[[ "$(tree_of ada)" == "$ADA_BEFORE" ]] || fail "unlisted ada's home changed (a single byte, incl. .git/refs)"
[[ "$(as ada git -C "$(wt_of ada clean)" rev-parse HEAD)" == "$ADA_HEAD" && "$ADA_HEAD" != "$UP2" ]] || fail "unlisted ada's clone moved"
pass "an unlisted (human) account's whole home is byte-identical, .git included"

# e. planted hooks / config do not run
[[ "$(cfg rex hooks rev-parse HEAD)" == "$UP2" ]] || fail "the hooks clone (clean, behind) did not fast-forward"
[[ ! -e $MARK/fsmonitor ]] || fail "core.fsmonitor ran"
bad="$(hook_markers)"
[[ -z "$bad" ]] || fail "a planted hook ran: $bad"
pass "a planted .git/hooks/*, a core.hooksPath at an evil hook dir and a core.fsmonitor program did NOT run (while the clone still fast-forwarded)"

# f. path attacks refused, the rest carried on
[[ "$(rgit -C /srv/victim rev-parse HEAD)" == "$VICTIM_HEAD" ]] || fail "the symlink target was moved"
grep -q 'ik-ruby:symlink: .* is a symlink; refusing' "$OUT" || fail "the symlinked clone was not refused: $(grep 'ik-ruby:symlink' "$OUT")"
[[ "$(rgit -C "$(wt_of ruby foreign)" rev-parse HEAD)" == "$FOREIGN_HEAD" ]] || fail "the foreign-owned clone moved"
grep -q 'ik-ruby:foreign: .* not a directory owned by the account; refusing' "$OUT" || fail "the foreign-owned clone was not refused"
pass "a symlinked clone path and a clone not owned by the account are refused; their targets are untouched"

# g. the hanging filter was bounded, did not stop the others, and left HEAD alone
[[ "$(cfg rex slow rev-parse HEAD)" != "$UP2" ]] || fail "the hanging-filter clone moved?"
grep -q 'ik-rex:slow: fast-forward failed' "$OUT" || fail "the timed-out fast-forward was not reported"
[[ ! -e "$(wt_of rex slow)/.git/index.lock" ]] || fail "a stale index.lock was left behind"
pass "a clone whose own config hangs git is cut off by the timeout (${ELAPSED}s total), reported, and the run carries on"

# h. root never ran git inside an account's clone
n_root="$(awk -F'\t' -v p="$PRINC" '$1==0 && (index($2,p)||index($3,p))' "$GITLOG" | wc -l)"
n_acct="$(awk -F'\t' -v p="$PRINC" '$1!=0 && (index($2,p)||index($3,p))' "$GITLOG" | wc -l)"
[[ "$n_acct" -gt 10 ]] || fail "control: the git shim saw only $n_acct account git calls in the clones (it logs nothing?)"
[[ "$n_root" -eq 0 ]] || fail "root ran git inside an account path: $(awk -F'\t' -v p="$PRINC" '$1==0 && (index($2,p)||index($3,p))' "$GITLOG" | head -3)"
grep -q 'core.hooksPath=/dev/null' "$GITLOG" || fail "the clone git calls did not pass core.hooksPath=/dev/null"
v1="$(awk -F'\t' -v p="$PRINC" '$1!=0 && (index($2,p)||index($3,p))' "$GITLOG" | grep -vc 'core.hooksPath=/dev/null' || true)"
v2="$(awk -F'\t' -v p="$PRINC" '$1!=0 && (index($2,p)||index($3,p))' "$GITLOG" | grep -vc 'core.fsmonitor=false' || true)"
[[ "$v1" == 0 ]] || fail "$v1 account git calls ran without core.hooksPath=/dev/null"
[[ "$v2" == 0 ]] || fail "$v2 account git calls ran without core.fsmonitor=false"
pass "all $n_acct git calls inside clones ran as the account (0 as root), each with hooksPath=/dev/null and fsmonitor off"

# ---------------------------------------------------------- 9. second run

advance 2
rm -f "$(wt_of ruby symlink)"; rm -rf /srv/victim; chown ik-ruby "$(wt_of ruby foreign)"
cfg rex dirty checkout -q -- README.md; rm -f "$(wt_of rex untracked)/scratch.tmp"
cfg rex slow config --unset filter.slow.smudge
refr
[[ $RC -eq 0 ]] || fail "second run exited $RC (the refusals were fixed)"
for n in clean dirty untracked hooks; do
  [[ "$(cfg rex $n rev-parse HEAD)" == "$(git -C $W rev-parse HEAD)" ]] || fail "rex's $n clone did not catch up once it was clean/unblocked"
done
[[ "$(cfg rex diverged log -1 --format=%s)" == "own work" ]] || fail "diverged clone changed on the second run"
# A timeout can land mid-checkout and leave the work tree part-updated; that
# clone then reads as dirty and is left strictly alone from then on.
[[ "$(cfg rex slow rev-parse HEAD)" != "$(git -C $W rev-parse HEAD)" ]] || fail "the interrupted clone moved"
grep -q 'ik-rex:slow: has uncommitted' "$OUT" || fail "the interrupted clone was not left alone as dirty"
[[ -z "$(hook_markers)" && ! -e $MARK/fsmonitor ]] || fail "a planted hook ran on the second run"
refr; grep -q 'ik-rex:clean: rex/main is up to date' "$OUT" || fail "an up-to-date clone was not recognised"
pass "once a clone is clean/unblocked the next run catches it up; a second run on current clones is a no-op"

# ------------------------------------------- 10. the unit as systemd runs it

cp "$PROFILE" "$INSTALL/.profile.env"; chmod 600 "$INSTALL/.profile.env"
advance 3
cmd="$(sed -n 's/^ExecStart=//p' $S)"
( set -a; eval "$(sed -n 's/^Environment=/export /p' $S)"; set +a; cd /; env -u IKENGA_REFRESH_GIT_TIMEOUT $cmd >"$OUT" 2>&1 ) || true
[[ "$(mirror_head clean)" == "$(git -C $W rev-parse HEAD)" ]] || fail "running the unit's ExecStart did not refresh the mirrors: $(cat "$OUT")"
[[ "$(cfg rex clean rev-parse HEAD)" == "$(git -C $W rev-parse HEAD)" ]] || fail "running the unit's ExecStart did not fast-forward rex"
pass "the unit's own ExecStart (stable copy, stored trusted profile) refreshes the mirrors and fast-forwards rex"

chmod 666 "$INSTALL/.profile.env"
RC=0; env -u IKENGA_REFRESH_GIT_TIMEOUT $cmd >"$OUT" 2>&1 || RC=$?
[[ $RC -ne 0 ]] || fail "the stable copy used a group/world-writable stored profile"
pass "the unit refuses an untrusted stored profile"

echo "==> [Container] ALL PROJECTS-REFRESH TESTS PASSED"
