-- Closes, in the feed, every git request that was settled before settling wrote a feed line.
--
-- `vcs::settle` and `vcs::settle_by_record` now append `vcs_request_settled` under the request's
-- subject. Before that they wrote nothing, so a request whose last line was
-- `vcs_resolution_started` drew on the Feed's trace as a resolution still going, for as long as
-- the window reached back -- days, for merges long since landed, superseded or dismissed. One
-- line per settled request that has a feed sequence at all, stamped at its settlement, so the
-- trace ends the bar where it really ended, in `vcs::settled_words`' own words. Only rows with no
-- such line yet.
INSERT INTO feed (project_id, kind, summary, run_id, subject, created_at)
SELECT r.project_id,
       'vcs_request_settled',
       'vcs request ' || r.id || ' no longer needs a person: ' || CASE r.settled_reason
           WHEN 'superseded' THEN 'a later request on the same merge succeeded'
           WHEN 'resolved' THEN 'a conflict resolution landed it under another request'
           WHEN 'merged' THEN 'its source is already in its target'
           WHEN 'source-gone' THEN 'its source branch is gone'
           ELSE r.settled_reason
       END,
       NULL,
       'vcs:' || r.id,
       r.settled_at
  FROM vcs_requests AS r
 WHERE r.settled_at IS NOT NULL
   AND EXISTS (SELECT 1 FROM feed AS f WHERE f.subject = 'vcs:' || r.id)
   AND NOT EXISTS (
       SELECT 1 FROM feed AS f WHERE f.subject = 'vcs:' || r.id AND f.kind = 'vcs_request_settled'
   )
 ORDER BY r.settled_at, r.id;
