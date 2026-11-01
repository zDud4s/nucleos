-- The node a continuation carries on from. `conductor::continuation::continue_node` opens at most one
-- sibling per handed-off node, and the unique index is what decides that when two calls race: the
-- loser's insert fails and it returns the winner's row. Walking the column back from a node gives the
-- length of its chain.
ALTER TABLE work_nodes ADD COLUMN continues_node_id TEXT;

CREATE UNIQUE INDEX work_nodes_continues ON work_nodes (continues_node_id)
    WHERE continues_node_id IS NOT NULL;
