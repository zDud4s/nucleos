# Node Handoff

<!-- READ-ONLY TEMPLATE. See packets/README.md for the contract.
  Written by the node to `<state_dir>/nodes/<root_id>/<node_id>.md` BEFORE it calls
  mcp__nucleos__node_close (the daemon refuses a close whose handoff file does not exist).
  The parent never reads this file unless Status is `blocked` or `failed`. -->

Node ID: <!-- as in the node packet -->
Root ID: <!-- as in the node packet -->
Status: <!-- done | blocked | handoff | failed -->

## Summary
<!-- at most 1500 characters; the SAME text passed to node_close. The only part a parent reads on `done`. -->

## What was done
<!-- children opened, one line each: `<node_id> <kind> <title> - <status> - <one-line result>`; for a leaf, the work itself -->

## Decisions
<!-- decisions taken in this node, one per line, mirroring those written through node_update; or `none` -->

## Files touched
<!-- paths, or `none`; all inside Owned paths -->

## Commands for the root
<!-- exact test/build commands the root must run for this node's work. Nodes never run them. Or `none`. -->

## Open items / next step
<!-- for `handoff`: enough for a fresh node to resume from the daemon tree (node_tree brief) without this file's author: what is done, what remains, in what order. Otherwise `none`. -->

## Blocked reason
<!-- for `blocked` or `failed`: what stopped the node and the decision or input needed, from whom; otherwise `none` -->
