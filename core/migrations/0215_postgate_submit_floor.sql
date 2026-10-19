-- The post-merge gate and its recheck / bisection probes claim a slot first and submit a verify
-- request second, and the ticket id is stored third. A crash between the second and third left a
-- request nobody knew about, and the next tick submitted another over it.
-- `submit_floor` is the newest `verify_requests.id` at the moment the slot (a gate or a probe) was
-- claimed, written by the claim itself. Any postgate request newer than it, for the same project,
-- was submitted under that claim, so the next tick adopts it instead of submitting again. NULL (a
-- claim made before this column) adopts nothing, which is the old behaviour. No CHECK constraints,
-- as in 0180, 0182, 0184 and 0186.
ALTER TABLE postgate_state ADD COLUMN submit_floor INTEGER;
