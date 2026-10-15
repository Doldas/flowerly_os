-- One foreground Flow per shared state database, including writers using
-- separate SQLite connections and writers bypassing Store::activate_flow.
-- Existing conflicting rows deliberately fail migration rather than silently
-- picking a winner and rewriting user state.
CREATE UNIQUE INDEX idx_flows_single_foreground
    ON flows(state) WHERE state = 'FOREGROUND';
