#!/bin/bash
# provision.sh PROJECTS_ACCESS: restrict a project to named managed accounts.
# Run as root inside an ubuntu:24.04 container (test-projects-access-container.sh)
# with real git and real accounts.
#
# Accounts: ada and eve = people, rex and ruby = agents. Projects (local bare
# repos): hub (restricted to rex ruby: the deals/legal/mail repo), legal
# (restricted to ruby), fresh (restricted from its first run), open (unrestricted).
#
# Every probe of "can this account read it" runs as the account via `as`:
# setpriv --clear-groups, exactly the daemon's sessions, so only the mode bits
# and the named-user ACL entries can grant anything.
set -euo pipefail

T="$(mktemp -d)"
PROV=/root/prov
PROVISION=/work/provision.sh
PROFILE="$PROV/profile.env"
OUT="$T/out"
PROJ=/srv/ikenga/projects
PRINC=/var/lib/ikenga-test/principals
SD="$T/systemd"
INSTALL="$T/opt-ikenga"
NAMES=(hub legal fresh open)

fail() { echo "FAIL: $*" >&2; [[ -f "$OUT" ]] && { echo "--- last output ---" >&2; cat "$OUT" >&2; }; exit 1; }
pass() { echo "==> [Container] $* PASSED"; }
trap 'rm -rf "$T"' EXIT

# ------------------------------------------------------------------ host

mkacct() {   # name uid : mimics what `ikenga-server accounts create` leaves behind
  groupadd -g "$2" "ik-$1"
  useradd -u "$2" -g "$2" -M -d "$PRINC/$2/home" -s /bin/sh "ik-$1"
  install -d -m 0700 -o "$2" -g "$2" "$PRINC/$2/home"
}
mkdir -p "$PRINC" "$SD" "$T/bin" "$T/sbin" "$INSTALL"
mkacct ada 20001; mkacct eve 20002; mkacct rex 20003; mkacct ruby 20004
home_of() { echo "$PRINC/$(id -u "ik-$1")/home"; }
wt_of() { echo "$(home_of "$1")/projects/$2"; }

as() {
  local n="$1" u; shift
  u="$(id -u "ik-$n")"
  ( cd / && setpriv --reuid="$u" --regid="$u" --clear-groups \
      env -i HOME="$PRINC/$u/home" PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 \
      GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.invalid GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.invalid "$@" )
}
# can_read <account> <project>: may the account read the mirror's files (not just see its name)?
can_read() { as "$1" cat "$PROJ/$2.git/HEAD" >/dev/null 2>&1 && as "$1" ls "$PROJ/$2.git/objects" >/dev/null 2>&1; }

# The container has no systemd; a no-op systemctl is enough (the refresh timer is off).
printf '#!/bin/bash\nexit 0\n' > "$T/bin/systemctl"; chmod +x "$T/bin/systemctl"

# ------------------------------------------------------------- fixtures

git config --global user.name test; git config --global user.email t@example.invalid
git config --global init.defaultBranch main
mkdir -p /fixtures-work; git init -q -b main /fixtures-work
W=/fixtures-work
echo hello > $W/README.md; echo one > $W/a.txt
git -C $W add -A && git -C $W commit -q -m first
mkdir -p /fixtures
for n in "${NAMES[@]}"; do git clone -q --bare $W "/fixtures/$n.git"; done
advance() {   # a second commit on every upstream
  echo "two $1" > $W/a.txt; git -C $W add -A && git -C $W commit -q -m "second $1"
  for n in "${NAMES[@]}"; do git -C $W push -q "/fixtures/$n.git" main; done
}

install -d -m 0700 "$PROV" "$INSTALL"
export IKENGA_SYSTEMD_DIR="$SD" IKENGA_PROVISION_STABLE="$T/sbin/ikenga-provision" IKENGA_PROJECTS_LOCK="$T/projects.lock" INSTALL_DIR="$INSTALL"
export PATH="$T/bin:$PATH"
RC=0
prov() { RC=0; "$PROVISION" sync-accounts --profile "$PROFILE" "$@" >"$OUT" 2>&1 || RC=$?; }
refr() { RC=0; "$PROVISION" refresh-projects --profile "$PROFILE" "$@" >"$OUT" 2>&1 || RC=$?; }
# mkprofile <lines...> : a profile with the four accounts, the given projects, no timer
mkprofile() {
  { echo 'ACCOUNTS=(ada eve rex ruby)'; echo 'AGENT_ACCOUNTS=(rex ruby)'; echo 'PROJECTS_REFRESH_SCHEDULE=off'; printf '%s\n' "$@"; } > "$PROFILE"
  chmod 0600 "$PROFILE"
}
projects_line() {   # projects_line <names...> : PROJECTS=("n=file:///fixtures/n.git" ...)
  local n out='PROJECTS=('
  for n in "$@"; do out+="\"$n=file:///fixtures/$n.git\" "; done
  echo "$out)"
}

# acl_names <path>: the named-user ACL entries on one path, sorted, as "ik-rex ik-ruby "
acl_names() { getfacl -cp -- "$1" 2>/dev/null | sed -n 's/^user:\(ik-[^:]*\):.*/\1/p' | sort | tr '\n' ' '; }
# dacl_names <path>: the DEFAULT named-user ACL entries (what a new file inside would inherit)
dacl_names() { getfacl -cp -- "$1" 2>/dev/null | sed -n 's/^default:user:\(ik-[^:]*\):.*/\1/p' | sort | tr '\n' ' '; }
# acl_exact <project> <expected "ik-a ik-b "> [open] : EVERY path in the mirror carries exactly that set,
# and (unless "open", an unrestricted project) every DIRECTORY carries the same default ACL, so what a
# later fetch writes starts out closed; files carry no default ACL
acl_exact() {
  local repo="$PROJ/$1.git" want="$2" kind="${3:-restricted}" p got dgot dwant n=0
  while IFS= read -r -d '' p; do
    got="$(acl_names "$p")"; n=$((n+1))
    [[ "$got" == "$want" ]] || fail "$1: ACL on ${p#$PROJ/} is '${got}', wanted '${want}'"
    dgot="$(dacl_names "$p")"; dwant=""
    [[ -d "$p" && "$kind" == restricted ]] && dwant="$want"
    [[ "$dgot" == "$dwant" ]] || fail "$1: DEFAULT ACL on ${p#$PROJ/} is '${dgot}', wanted '${dwant}'"
    if [[ -d "$p" && "$kind" == restricted ]]; then
      getfacl -cp -- "$p" | grep -qx 'default:other::---' || fail "$1: ${p#$PROJ/} has no 'default:other::---'"
    fi
  done < <(find "$repo" -print0)
  [[ $n -gt 10 ]] || fail "$1: only $n paths checked; is the mirror empty?"
}
# no_other_bits <project> : no path in the mirror is readable/writable/searchable by "other"
no_other_bits() {
  local bad; bad="$(find "$PROJ/$1.git" -perm /007 -print -quit)"
  [[ -z "$bad" ]] || fail "$1: '$bad' still has permission bits for other"
}
head_of() { as "$1" git -C "$(wt_of "$1" "$2")" rev-parse HEAD; }   # as the account: git refuses a clone another uid owns

# ---------------------------------------------------- 1. validation errors

P="$(projects_line hub open)"
bad() {   # bad <expected message fragment> <extra profile lines...> : uses accounts ada eve rex ruby
  local want="$1"; shift
  mkprofile "$P" "$@"
  prov --dry-run
  [[ $RC -ne 0 ]] || fail "expected failure for: $want"
  grep -qF -- "$want" "$OUT" || fail "wanted '$want' in: $(cat "$OUT")"
}
bad "PROJECTS_ACCESS names 'hubb', which is not in PROJECTS" 'PROJECTS_ACCESS=("hubb=rex")'
bad "PROJECTS_ACCESS names 'hub rex', which is not in PROJECTS" 'PROJECTS_ACCESS=("hub rex=ruby")'
bad "PROJECTS_ACCESS names 'hub open', which is not in PROJECTS" 'PROJECTS_ACCESS=("hub open=rex")'
bad "PROJECTS_ACCESS names 'hub' twice" 'PROJECTS_ACCESS=("hub=rex" "hub=ruby")'
bad "PROJECTS_ACCESS 'hub' lists no accounts" 'PROJECTS_ACCESS=("hub=")'
bad "PROJECTS_ACCESS 'hub' lists no accounts" 'PROJECTS_ACCESS=("hub=   ")'
bad "must look like project=account" 'PROJECTS_ACCESS=("hub")'
bad "must look like project=account" 'PROJECTS_ACCESS=("=rex")'
bad "PROJECTS_ACCESS 'hub': 'Rex' is not a valid login name" 'PROJECTS_ACCESS=("hub=Rex")'
bad "PROJECTS_ACCESS 'hub': 'rex;id' is not a valid login name" 'PROJECTS_ACCESS=("hub=rex;id")'
bad "PROJECTS_ACCESS 'hub': 'agents' is reserved" 'PROJECTS_ACCESS=("hub=agents")'
bad "PROJECTS_ACCESS 'hub': 'root' is reserved" 'PROJECTS_ACCESS=("hub=rex root")'
bad "PROJECTS_ACCESS 'hub' names 'rex' twice" 'PROJECTS_ACCESS=("hub=rex rex")'
bad "PROJECTS_ACCESS 'hub' names 'zed', which is not a managed account" 'PROJECTS_ACCESS=("hub=rex zed")'
# PROJECTS_ACCESS narrows PROJECTS_MEMBERS; it cannot widen it.
bad "PROJECTS_ACCESS 'hub' names 'ruby', who is not in PROJECTS_MEMBERS" 'PROJECTS_MEMBERS=(rex)' 'PROJECTS_ACCESS=("hub=rex ruby")'
# With ACCOUNTS empty the managed set is discovered (ik-* in UID_RANGE): checked after that.
{ echo 'PROJECTS_REFRESH_SCHEDULE=off'; echo "$P"; echo 'PROJECTS_ACCESS=("hub=rex zed")'; } > "$PROFILE"
prov --dry-run
[[ $RC -ne 0 ]] && grep -qF "PROJECTS_ACCESS 'hub' names 'zed', which is not a managed account" "$OUT" || fail "an unmanaged account passed with ACCOUNTS empty (rc=$RC)"
refr --dry-run
[[ $RC -ne 0 ]] && grep -qF "names 'zed', which is not a managed account" "$OUT" || fail "refresh-projects accepted an unmanaged access account (rc=$RC)"
[[ ! -e $PROJ ]] || fail "a rejected profile created $PROJ"
pass "validation: typo'd / space-bypass / duplicate project, empty list, bad / reserved / duplicate / unmanaged account, and widening PROJECTS_MEMBERS all die before anything is written"

# ------------- 2. world-readable first (no PROJECTS_ACCESS): the state to fix

PALL="$(projects_line hub legal open)"
mkprofile 'PROJECTS_READ=world' "$PALL"
prov
[[ $RC -eq 0 ]] || fail "baseline world run exited $RC"
for n in hub legal open; do for a in ada eve rex ruby; do
  [[ -d "$(wt_of $a $n)/.git" ]] || fail "baseline: ik-$a has no clone of $n"
done; done
for a in ada eve rex ruby; do can_read $a hub || fail "baseline: ik-$a cannot read the world-readable hub"; done
[[ -z "$(acl_names $PROJ/hub.git)" ]] || fail "baseline: a world-readable mirror carries ACL entries"
pass "baseline: PROJECTS_READ=world mirrors everything world-readable and everyone gets every clone"

# -------- 3. restrict hub + legal: dry run first, then the real run fixes it

for a in ada; do echo "people-notes" > "$(wt_of $a hub)/MARK"; done
HUB_ADA_HEAD="$(head_of ada hub)"
mkprofile 'PROJECTS_READ=world' "$PALL" 'PROJECTS_ACCESS=("hub=rex ruby" "legal=ruby")'
prov --dry-run
[[ $RC -eq 0 ]] || fail "restricted dry run exited $RC"
grep -qF "project hub: access: rex ruby (mirror never world-readable" "$OUT" || fail "dry run does not show hub's access: $(cat "$OUT")"
grep -qF "project legal: access: ruby (" "$OUT" || fail "dry run does not show legal's access"
! grep -q 'project open: access' "$OUT" || fail "dry run claims open is restricted"
grep -qF "mirror $PROJ/hub.git locked down" "$OUT" || fail "dry run does not plan to lock the world-readable hub"
can_read ada hub || fail "the dry run already changed hub's permissions"
[[ -z "$(acl_names $PROJ/hub.git)" ]] || fail "the dry run already wrote an ACL"
pass "dry run shows each restricted project's access list and the planned lock-down, and writes nothing"

# a fresh project that is restricted from its very first run, under world: dry run lists clones only for members
mkprofile 'PROJECTS_READ=world' "$(projects_line hub legal fresh open)" 'PROJECTS_ACCESS=("hub=rex ruby" "legal=ruby" "fresh=rex")'
prov --dry-run
grep -qF "would: project fresh mirrored from file:///fixtures/fresh.git (access: rex)" "$OUT" || fail "dry run: fresh not shown with its access: $(cat "$OUT")"
grep -qF "would: clone ik-rex:fresh" "$OUT" || fail "dry run: no clone of fresh for rex"
for a in ada eve ruby; do ! grep -qF "clone ik-$a:fresh" "$OUT" || fail "dry run plans a fresh clone for ik-$a"; done
[[ ! -e $PROJ/fresh.git ]] || fail "the dry run created the fresh mirror"
pass "dry run: a new restricted project's clone is planned only for its accounts"

prov
[[ $RC -eq 0 ]] || fail "restricted run exited $RC"
no_other_bits hub; no_other_bits legal; no_other_bits fresh
acl_exact hub "ik-rex ik-ruby "
acl_exact legal "ik-ruby "
acl_exact fresh "ik-rex "
[[ -z "$(acl_names $PROJ/open.git)" ]] && [[ -n "$(find $PROJ/open.git -perm -004 -print -quit)" ]] || fail "open lost its world-readability"
for a in ada eve rex ruby; do can_read $a open || fail "ik-$a cannot read the unrestricted open"; done
can_read rex hub && can_read ruby hub || fail "an allowed account cannot read hub"
can_read ada hub && fail "ada can still read hub (world -> restricted was not fixed)"
can_read eve hub && fail "eve can read hub"
can_read rex legal && fail "rex can read legal"
can_read ruby legal || fail "ruby cannot read legal"
can_read ada fresh && fail "ada can read fresh"
as ada git -C "$PROJ/hub.git" log >/dev/null 2>&1 && fail "ada can run git log in hub"
as rex git -C "$PROJ/hub.git" log >/dev/null 2>&1 || fail "rex cannot run git log in hub"
pass "world -> restricted: hub/legal/fresh lose every 'other' bit, carry read ACLs for exactly their accounts (every path), open stays world-readable"

# clones: only members get one; nobody's existing clone is touched
for n in hub; do for a in rex ruby; do [[ -d "$(wt_of $a $n)/.git" ]] || fail "ik-$a lost its $n clone"; done; done
[[ -d "$(wt_of rex fresh)/.git" ]] || fail "rex has no clone of fresh"
for a in ada eve ruby; do [[ ! -e "$(wt_of $a fresh)" ]] || fail "ik-$a got a clone of fresh"; done
[[ -f "$(wt_of ada hub)/MARK" && "$(head_of ada hub)" == "$HUB_ADA_HEAD" ]] || fail "ada's pre-existing hub clone was touched"
[[ -d "$(wt_of eve hub)/.git" && -d "$(wt_of rex legal)/.git" ]] || fail "an existing clone of a now-excluded account was deleted"
grep -qF "ik-ada:hub: not in PROJECTS_ACCESS, but already has a clone; left as it is" "$OUT" || fail "no note about ada's old hub clone: $(cat "$OUT")"
pass "clones are created only for the effective set; an excluded account's existing clone is left alone and reported"

prov
[[ $RC -eq 0 ]] || fail "rerun exited $RC"
! grep -qE 'locked down|clone ik-' "$OUT" || fail "rerun changed something: $(cat "$OUT")"
pass "a rerun changes nothing"

# ----------------- 4. narrowing revokes; widening restores; exactness holds

RUBY_HUB_HEAD="$(head_of ruby hub)"
mkprofile 'PROJECTS_READ=world' "$(projects_line hub legal fresh open)" 'PROJECTS_ACCESS=("hub=rex" "legal=ruby" "fresh=rex")'
prov
[[ $RC -eq 0 ]] || fail "narrowing exited $RC"
acl_exact hub "ik-rex "
can_read ruby hub && fail "ruby can still read hub after being removed from its access list"
can_read rex hub || fail "rex lost hub"
grep -qF "mirror $PROJ/hub.git locked down" "$OUT" || fail "narrowing reported no change: $(cat "$OUT")"
[[ -d "$(wt_of ruby hub)/.git" && "$(head_of ruby hub)" == "$RUBY_HUB_HEAD" ]] || fail "ruby's hub clone was touched when it was removed"
grep -qF "ik-ruby:hub: not in PROJECTS_ACCESS, but already has a clone" "$OUT" || fail "no note about ruby's hub clone"
pass "narrowing revokes the removed account's ACL on every path (stale entries are dropped); its clone stays"

mkprofile 'PROJECTS_READ=world' "$(projects_line hub legal fresh open)" 'PROJECTS_ACCESS=("hub=rex ruby" "legal=ruby" "fresh=rex")'
prov
[[ $RC -eq 0 ]] || fail "widening exited $RC"
acl_exact hub "ik-rex ik-ruby "
can_read ruby hub || fail "ruby did not get hub back"
no_other_bits hub
pass "widening grants the added account again, still no 'other' bits"

# PROJECTS_READ=members behaves the same for a restricted project
mkprofile 'PROJECTS_READ=members' "$(projects_line hub legal fresh open)" 'PROJECTS_ACCESS=("hub=rex ruby" "legal=ruby" "fresh=rex")'
prov
[[ $RC -eq 0 ]] || fail "members-mode run exited $RC"
acl_exact hub "ik-rex ik-ruby "; acl_exact open "ik-ada ik-eve ik-rex ik-ruby " open; acl_exact fresh "ik-rex "
can_read ada hub && fail "members mode: ada can read hub"
can_read ada open || fail "members mode: ada cannot read open"
pass "PROJECTS_READ=members: restricted projects get exactly their accounts, unrestricted ones all members"

# What a file created inside a restricted mirror by ANYTHING (a fetch with a loose umask, a stray root
# process) starts as: the default ACL closes it, whatever the umask. (The 0750 top directory already
# keeps every non-member out of the tree; this is the second wall.)
(umask 000; mkdir "$PROJ/hub.git/objects/zz"; : > "$PROJ/hub.git/objects/zz/f")
[[ -z "$(find "$PROJ/hub.git/objects/zz" -perm /007 -print -quit)" ]] || fail "a file created with umask 000 in hub got 'other' bits: $(stat -c '%A' $PROJ/hub.git/objects/zz/f)"
[[ "$(acl_names $PROJ/hub.git/objects/zz/f)" == "ik-rex ik-ruby " && "$(acl_names $PROJ/hub.git/objects/zz)" == "ik-rex ik-ruby " ]] || fail "a new file/dir in hub did not inherit the readers: '$(acl_names $PROJ/hub.git/objects/zz/f)'"
as rex cat "$PROJ/hub.git/objects/zz/f" >/dev/null 2>&1 || fail "an allowed account cannot read a new file in hub"
as ada cat "$PROJ/hub.git/objects/zz/f" >/dev/null 2>&1 && fail "ada can read a new file in hub"
rm -rf "$PROJ/hub.git/objects/zz"
pass "default ACLs: anything created inside a restricted mirror (even with umask 000) inherits exactly the allowed readers and no 'other' access"

# Dropping a project from PROJECTS_ACCESS opens it up again: members mode = every member, defaults gone;
# world mode = world-readable, no ACL at all.
mkprofile 'PROJECTS_READ=members' "$(projects_line hub legal fresh open)" 'PROJECTS_ACCESS=("hub=rex ruby" "fresh=rex")'
prov
[[ $RC -eq 0 ]] || fail "dropping legal from PROJECTS_ACCESS exited $RC"
acl_exact legal "ik-ada ik-eve ik-rex ik-ruby " open
can_read ada legal || fail "legal is still closed to ada after it left PROJECTS_ACCESS (members)"
acl_exact hub "ik-rex ik-ruby "
mkprofile 'PROJECTS_READ=world' "$(projects_line hub legal fresh open)" 'PROJECTS_ACCESS=("hub=rex ruby" "fresh=rex")'
prov
[[ $RC -eq 0 ]] || fail "world-mode run after dropping legal exited $RC"
[[ -z "$(acl_names $PROJ/legal.git)" && -z "$(dacl_names $PROJ/legal.git)" ]] || fail "legal still carries ACL entries in world mode"
[[ -z "$(find $PROJ/legal.git ! -perm -004 ! -type l -print -quit)" ]] || fail "legal is not world-readable again"
can_read eve legal || fail "eve cannot read legal (world)"
can_read ada hub && fail "ada can read hub in world mode: restricted must beat PROJECTS_READ=world"
acl_exact hub "ik-rex ik-ruby "
mkprofile 'PROJECTS_READ=world' "$(projects_line hub legal fresh open)" 'PROJECTS_ACCESS=("hub=rex ruby" "legal=ruby" "fresh=rex")'
prov
[[ $RC -eq 0 ]] || fail "re-restricting legal exited $RC"
acl_exact legal "ik-ruby "; no_other_bits legal
pass "a project dropped from PROJECTS_ACCESS is opened again (members: all members, no defaults; world: world-readable, no ACL); a restricted one stays closed under world; re-restricting closes it"

# -------------- 5. refresh-projects: fast-forward is FASTFORWARD ∩ effective set

advance 1
mkprofile 'PROJECTS_READ=world' 'PROJECTS_FASTFORWARD=(rex ruby ada)' "$(projects_line hub legal fresh open)" 'PROJECTS_ACCESS=("hub=rex" "legal=ruby" "fresh=rex")'
OLD_RUBY_HUB="$(head_of ruby hub)"; OLD_ADA_HUB="$(head_of ada hub)"; OLD_EVE_OPEN="$(head_of eve open)"
touch "$T/before-fetch"; sleep 1.1
refr
[[ $RC -eq 0 ]] || fail "refresh-projects exited $RC"
NEW="$(git --git-dir=/fixtures/hub.git rev-parse main)"
[[ "$(head_of rex hub)" == "$NEW" ]] || fail "rex (in FASTFORWARD and in hub's access list) was not fast-forwarded"
[[ "$(head_of ruby hub)" == "$OLD_RUBY_HUB" ]] || fail "ruby is in FASTFORWARD but not in hub's access list, yet its hub clone moved"
[[ "$(head_of ada hub)" == "$OLD_ADA_HUB" ]] || fail "ada is in FASTFORWARD but not in hub's access list, yet its hub clone moved"
grep -qF "ik-ruby:hub: not in PROJECTS_ACCESS; skipped" "$OUT" || fail "no skip note for ruby:hub: $(cat "$OUT")"
grep -qF "ik-ada:hub: not in PROJECTS_ACCESS; skipped" "$OUT" || fail "no skip note for ada:hub"
grep -qF "project hub: access: rex (" "$OUT" || fail "refresh-projects does not show the access list"
for a in rex ruby ada; do [[ "$(head_of $a open)" == "$NEW" ]] || fail "ik-$a's open clone (unrestricted) was not fast-forwarded"; done
[[ "$(head_of eve open)" == "$OLD_EVE_OPEN" ]] || fail "eve is not in FASTFORWARD but her clone moved"
[[ "$(head_of ruby legal)" == "$NEW" && "$(head_of rex legal)" != "$NEW" ]] || fail "legal: only ruby should have moved (ruby=$(head_of ruby legal) rex=$(head_of rex legal))"
no_other_bits hub; acl_exact hub "ik-rex "
can_read ada hub && fail "refresh made hub readable again"
# Everything the fetch just wrote (packs, indexes, refs, directories): a non-member (ada, ruby) can
# neither list, read nor use any of it; the allowed account (rex) can read all of it.
mapfile -d '' NEWF < <(find "$PROJ/hub.git" -newer "$T/before-fetch" -print0)
NF_FILES=0
for f in "${NEWF[@]}"; do
  [[ -f "$f" ]] && NF_FILES=$((NF_FILES+1))
  for a in ada ruby eve; do
    if [[ -d "$f" ]]; then as $a ls -- "$f" >/dev/null 2>&1 && fail "ik-$a can list ${f#$PROJ/}, written by the last fetch"
    else as $a cat -- "$f" >/dev/null 2>&1 && fail "ik-$a can read ${f#$PROJ/}, written by the last fetch"; fi
  done
  if [[ -f "$f" ]]; then as rex cat -- "$f" >/dev/null 2>&1 || fail "rex cannot read ${f#$PROJ/}, written by the last fetch"; fi
done
[[ $NF_FILES -ge 3 ]] || fail "the refresh wrote only $NF_FILES new files into hub; the check proves nothing"
NEWPACK="$(find "$PROJ/hub.git/objects" -newer "$T/before-fetch" \( -name '*.pack' -o -name '*.idx' -o -type f -path '*/objects/??/*' \) -print -quit)"
[[ -n "$NEWPACK" ]] || fail "the fetch wrote no new object/pack file to probe"
for a in ada ruby eve; do
  as $a ls "$PROJ/hub.git" >/dev/null 2>&1 && fail "ik-$a can list the hub mirror"
  as $a cat "$PROJ/hub.git/HEAD" >/dev/null 2>&1 && fail "ik-$a can read hub's HEAD"
  as $a git ls-remote "$PROJ/hub.git" >/dev/null 2>&1 && fail "ik-$a can git ls-remote the hub mirror after a fetch"
  as $a git --git-dir "$PROJ/hub.git" cat-file -p "$(git --git-dir /fixtures/hub.git rev-parse main)" >/dev/null 2>&1 && fail "ik-$a can read the new commit out of the hub mirror"
done
as rex git ls-remote "$PROJ/hub.git" >/dev/null 2>&1 || fail "rex cannot git ls-remote the hub mirror"
pass "after a fetch into a restricted mirror ($NF_FILES new files, packs included): every new file/dir is unreadable to the non-members (ls, cat, git ls-remote, cat-file all fail) and readable to the allowed account"
pass "refresh-projects fast-forwards only FASTFORWARD ∩ access; the refetch keeps a restricted mirror closed (no 'other' bits, exact ACL)"

# A restricted clone is closed at its top directory, so it stays private even
# when the member's home is not 0700 (an adopted account can keep a looser one).
[[ -d "$(wt_of rex hub)" ]] || fail "rex has no hub clone to probe"
[[ "$(stat -c %a "$(wt_of rex hub)")" == 700 ]] || fail "rex's restricted hub clone is mode $(stat -c %a "$(wt_of rex hub)"), not 700"
[[ "$(( 8#$(stat -c %a "$(wt_of rex open)") & 8#005 ))" -ne 0 ]] || fail "rex's unrestricted open clone was closed too"
chmod 0755 "$(home_of rex)"
as rex chmod 0775 "$(wt_of rex hub)"
as ada ls "$(wt_of rex hub)" >/dev/null 2>&1 || fail "setup: the loosened clone should be listable before the re-run"
prov
[[ $RC -eq 0 ]] || fail "sync-accounts exited $RC"
grep -qF "close ik-rex:hub clone to other users (was mode 775)" "$OUT" || fail "no report of closing rex's loosened hub clone"
[[ "$(stat -c %a "$(wt_of rex hub)")" == 700 ]] || fail "the re-run left rex's hub clone open"
as ada ls "$(wt_of rex hub)" >/dev/null 2>&1 && fail "ada can list rex's restricted clone through a 0755 home"
as rex git -C "$(wt_of rex hub)" status >/dev/null 2>&1 || fail "rex cannot use its own clone after it was closed"
chmod 0700 "$(home_of rex)"
pass "a restricted project's clone is mode 700 and is re-closed on every run, even under a 0755 home"

echo "==> [Container] all PROJECTS_ACCESS checks PASSED"
