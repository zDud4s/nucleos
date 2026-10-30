---
name: conduct
description: Use when you are a top-level session (IDE, tunnel, terminal) given a development task — decide how to route it by size instead of executing it yourself. Covers the small/medium/large table, the `manual:` escape, waiting and resuming.
---

# Conduct

**The top-level session does not execute: it launches.** Your job is to pick the route, hand the
work to a dispatched agent or to the daemon, and report. You do not edit source, run builds or
read implementation files to do the work yourself.

## Route by size

| Size | Route |
|---|---|
| Small: one file, no decision | `Agent` with `wf-leaf` (fallback if absent: `wf-executor`) |
| Medium: one feature, 2-6 files | `Agent` + the `orchestrate` skill |
| Large: several specs, or "finish everything" | `tree_launch` and `spec_finish` (daemon tools; if available) |

`tree_launch` and `spec_finish` hand the work to a daemon run: the tree's driver and its nodes
run as daemon runs, not in your session, so closing the laptop or dropping a tunnel does not
stop them. If those tools are not offered in your session, fall back to `Agent` + `orchestrate`
and say so.

## Escape

The owner starts a prompt with `manual:` to opt out of the conductor's routing for that one
prompt. Only an owner prompt can set it; never write the flag yourself.

## Waiting

Before a long wait (a cache-expensive pause, a tree running for hours), call `wait_advice`, if
available, and follow it. If it is not offered, keep the session short and let the daemon notify.

## Resuming

To resume a session, ask the daemon: `POST /conductor/resume {session_id}`. Never run an
interactive `claude --resume`: it can sit for hours on a trust dialog with nobody told.
