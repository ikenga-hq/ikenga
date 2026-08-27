---
'ikenga-desktop': patch
---

Add a launch smoke gate to the release pipeline, and stop the iyke bridge
swallowing its own failures.

**The gate.** Every check in the pipeline verified that the code compiles and
its units pass; none of them ever started the app. That is how v0.8.0 shipped
with a dead iyke FE⇄backend channel and no terminal able to spawn, while
typecheck, both cargo checks, 786 Rust tests, 804 frontend tests, CI and all
four release legs were green (#140). `scripts/launch-smoke-gate.ts` now launches
the built binary on the Linux release leg under `xvfb-run` and probes
`GET /iyke/dom`, which round-trips backend → FE listener → `invoke` → backend
and so fails on exactly that class of break. Verified against both artifacts:
passes on v0.7.2, fails on v0.8.0 with `iyke://dom-request timed out after
5000ms`. A smoke failure fails the build job, so the release stays a draft and
never becomes `Latest`.

Two things the gate has to work around, both of which would otherwise make it
lie. A virgin data dir is a first run, and a first run renders `/onboarding`
without the Workspace chrome that mounts the bridge — so it boots once to let
the app migrate its database, seeds onboarding as complete, then relaunches and
probes that second boot. And `control.json` outlives the process it describes,
so it is deleted before the relaunch; otherwise the probe dials a dead port and
reports a connection error indistinguishable from a hung frontend.

**Diagnosability.** Every `.catch(() => {})` around an iyke resolve now names
the failing command, so "the listener never registered" and "the listener ran
and could not answer" stop producing an identical backend timeout. The console
instrumentation is installed at frame 0 in `main.tsx` rather than by
`useIykeBridge`, which removes the circularity that left `/iyke/logs` empty and
healthy-looking precisely when the bridge was broken.

**Capability snapshots.** `pkg_capability_snapshots` rows are now torn down with
the install record, so a dev mount's implicit approval can no longer outlive the
mount and silently pre-approve a later real install of the same pkg id (#144).
