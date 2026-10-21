-- Phase 2 of the channels work: every conversation column that holds a Telegram chat now carries
-- the `telegram:` channel prefix, `telegram:<chat>[:<thread>]` (ConversationKey, contract.rs).
--
-- A value is rewritten only when it has the Telegram shape -- `-?digits` or `-?digits:digits` --
-- AND is not a shell chat (`chats.chat_id`). SQLite has no regex, so the shape is spelled with
-- GLOB, repeated literally for each of the 12 columns below (a migration cannot define a view).
-- Every reference to the column is qualified by the table of its UPDATE: an unqualified `chat_id`
-- inside the `NOT EXISTS` resolves to `chats.chat_id` and turns the test into a tautology.
--
-- NULL, shell UUIDs, `abc`-like values and already-prefixed values do not match the shape, so a
-- second application changes nothing. Primary keys (`assistant_sessions.chat_id`,
-- `chat_tokens.chat_id`) cannot collide: the new value has a prefix no old value had.
-- `origin` is not touched here (0235).

UPDATE runs SET chat_id = 'telegram:' || chat_id
 WHERE runs.chat_id IS NOT NULL
   AND (
        (runs.chat_id <> '' AND substr(runs.chat_id, 1, 1) = '-' AND length(runs.chat_id) > 1
           AND substr(runs.chat_id, 2) NOT GLOB '*[^0-9]*')
     OR (runs.chat_id <> '' AND runs.chat_id NOT GLOB '*[^0-9]*')
     OR (runs.chat_id NOT GLOB '*:*:*' AND instr(runs.chat_id, ':') > 1
           AND substr(runs.chat_id, instr(runs.chat_id, ':') + 1) <> ''
           AND substr(runs.chat_id, instr(runs.chat_id, ':') + 1) NOT GLOB '*[^0-9]*'
           AND ((substr(runs.chat_id, 1, 1) = '-' AND instr(runs.chat_id, ':') > 2
                   AND substr(runs.chat_id, 2, instr(runs.chat_id, ':') - 2) NOT GLOB '*[^0-9]*')
                OR substr(runs.chat_id, 1, instr(runs.chat_id, ':') - 1) NOT GLOB '*[^0-9]*'))
       )
   AND NOT EXISTS (SELECT 1 FROM chats WHERE chats.chat_id = runs.chat_id);

UPDATE runs SET launched_by_chat_id = 'telegram:' || launched_by_chat_id
 WHERE runs.launched_by_chat_id IS NOT NULL
   AND (
        (runs.launched_by_chat_id <> '' AND substr(runs.launched_by_chat_id, 1, 1) = '-' AND length(runs.launched_by_chat_id) > 1
           AND substr(runs.launched_by_chat_id, 2) NOT GLOB '*[^0-9]*')
     OR (runs.launched_by_chat_id <> '' AND runs.launched_by_chat_id NOT GLOB '*[^0-9]*')
     OR (runs.launched_by_chat_id NOT GLOB '*:*:*' AND instr(runs.launched_by_chat_id, ':') > 1
           AND substr(runs.launched_by_chat_id, instr(runs.launched_by_chat_id, ':') + 1) <> ''
           AND substr(runs.launched_by_chat_id, instr(runs.launched_by_chat_id, ':') + 1) NOT GLOB '*[^0-9]*'
           AND ((substr(runs.launched_by_chat_id, 1, 1) = '-' AND instr(runs.launched_by_chat_id, ':') > 2
                   AND substr(runs.launched_by_chat_id, 2, instr(runs.launched_by_chat_id, ':') - 2) NOT GLOB '*[^0-9]*')
                OR substr(runs.launched_by_chat_id, 1, instr(runs.launched_by_chat_id, ':') - 1) NOT GLOB '*[^0-9]*'))
       )
   AND NOT EXISTS (SELECT 1 FROM chats WHERE chats.chat_id = runs.launched_by_chat_id);

UPDATE assistant_sessions SET chat_id = 'telegram:' || chat_id
 WHERE assistant_sessions.chat_id IS NOT NULL
   AND (
        (assistant_sessions.chat_id <> '' AND substr(assistant_sessions.chat_id, 1, 1) = '-' AND length(assistant_sessions.chat_id) > 1
           AND substr(assistant_sessions.chat_id, 2) NOT GLOB '*[^0-9]*')
     OR (assistant_sessions.chat_id <> '' AND assistant_sessions.chat_id NOT GLOB '*[^0-9]*')
     OR (assistant_sessions.chat_id NOT GLOB '*:*:*' AND instr(assistant_sessions.chat_id, ':') > 1
           AND substr(assistant_sessions.chat_id, instr(assistant_sessions.chat_id, ':') + 1) <> ''
           AND substr(assistant_sessions.chat_id, instr(assistant_sessions.chat_id, ':') + 1) NOT GLOB '*[^0-9]*'
           AND ((substr(assistant_sessions.chat_id, 1, 1) = '-' AND instr(assistant_sessions.chat_id, ':') > 2
                   AND substr(assistant_sessions.chat_id, 2, instr(assistant_sessions.chat_id, ':') - 2) NOT GLOB '*[^0-9]*')
                OR substr(assistant_sessions.chat_id, 1, instr(assistant_sessions.chat_id, ':') - 1) NOT GLOB '*[^0-9]*'))
       )
   AND NOT EXISTS (SELECT 1 FROM chats WHERE chats.chat_id = assistant_sessions.chat_id);

UPDATE chat_tokens SET chat_id = 'telegram:' || chat_id
 WHERE chat_tokens.chat_id IS NOT NULL
   AND (
        (chat_tokens.chat_id <> '' AND substr(chat_tokens.chat_id, 1, 1) = '-' AND length(chat_tokens.chat_id) > 1
           AND substr(chat_tokens.chat_id, 2) NOT GLOB '*[^0-9]*')
     OR (chat_tokens.chat_id <> '' AND chat_tokens.chat_id NOT GLOB '*[^0-9]*')
     OR (chat_tokens.chat_id NOT GLOB '*:*:*' AND instr(chat_tokens.chat_id, ':') > 1
           AND substr(chat_tokens.chat_id, instr(chat_tokens.chat_id, ':') + 1) <> ''
           AND substr(chat_tokens.chat_id, instr(chat_tokens.chat_id, ':') + 1) NOT GLOB '*[^0-9]*'
           AND ((substr(chat_tokens.chat_id, 1, 1) = '-' AND instr(chat_tokens.chat_id, ':') > 2
                   AND substr(chat_tokens.chat_id, 2, instr(chat_tokens.chat_id, ':') - 2) NOT GLOB '*[^0-9]*')
                OR substr(chat_tokens.chat_id, 1, instr(chat_tokens.chat_id, ':') - 1) NOT GLOB '*[^0-9]*'))
       )
   AND NOT EXISTS (SELECT 1 FROM chats WHERE chats.chat_id = chat_tokens.chat_id);

UPDATE chat_notices SET chat_id = 'telegram:' || chat_id
 WHERE chat_notices.chat_id IS NOT NULL
   AND (
        (chat_notices.chat_id <> '' AND substr(chat_notices.chat_id, 1, 1) = '-' AND length(chat_notices.chat_id) > 1
           AND substr(chat_notices.chat_id, 2) NOT GLOB '*[^0-9]*')
     OR (chat_notices.chat_id <> '' AND chat_notices.chat_id NOT GLOB '*[^0-9]*')
     OR (chat_notices.chat_id NOT GLOB '*:*:*' AND instr(chat_notices.chat_id, ':') > 1
           AND substr(chat_notices.chat_id, instr(chat_notices.chat_id, ':') + 1) <> ''
           AND substr(chat_notices.chat_id, instr(chat_notices.chat_id, ':') + 1) NOT GLOB '*[^0-9]*'
           AND ((substr(chat_notices.chat_id, 1, 1) = '-' AND instr(chat_notices.chat_id, ':') > 2
                   AND substr(chat_notices.chat_id, 2, instr(chat_notices.chat_id, ':') - 2) NOT GLOB '*[^0-9]*')
                OR substr(chat_notices.chat_id, 1, instr(chat_notices.chat_id, ':') - 1) NOT GLOB '*[^0-9]*'))
       )
   AND NOT EXISTS (SELECT 1 FROM chats WHERE chats.chat_id = chat_notices.chat_id);

UPDATE team_runs SET report_to_chat_id = 'telegram:' || report_to_chat_id
 WHERE team_runs.report_to_chat_id IS NOT NULL
   AND (
        (team_runs.report_to_chat_id <> '' AND substr(team_runs.report_to_chat_id, 1, 1) = '-' AND length(team_runs.report_to_chat_id) > 1
           AND substr(team_runs.report_to_chat_id, 2) NOT GLOB '*[^0-9]*')
     OR (team_runs.report_to_chat_id <> '' AND team_runs.report_to_chat_id NOT GLOB '*[^0-9]*')
     OR (team_runs.report_to_chat_id NOT GLOB '*:*:*' AND instr(team_runs.report_to_chat_id, ':') > 1
           AND substr(team_runs.report_to_chat_id, instr(team_runs.report_to_chat_id, ':') + 1) <> ''
           AND substr(team_runs.report_to_chat_id, instr(team_runs.report_to_chat_id, ':') + 1) NOT GLOB '*[^0-9]*'
           AND ((substr(team_runs.report_to_chat_id, 1, 1) = '-' AND instr(team_runs.report_to_chat_id, ':') > 2
                   AND substr(team_runs.report_to_chat_id, 2, instr(team_runs.report_to_chat_id, ':') - 2) NOT GLOB '*[^0-9]*')
                OR substr(team_runs.report_to_chat_id, 1, instr(team_runs.report_to_chat_id, ':') - 1) NOT GLOB '*[^0-9]*'))
       )
   AND NOT EXISTS (SELECT 1 FROM chats WHERE chats.chat_id = team_runs.report_to_chat_id);

UPDATE chat_relays SET from_chat_id = 'telegram:' || from_chat_id
 WHERE chat_relays.from_chat_id IS NOT NULL
   AND (
        (chat_relays.from_chat_id <> '' AND substr(chat_relays.from_chat_id, 1, 1) = '-' AND length(chat_relays.from_chat_id) > 1
           AND substr(chat_relays.from_chat_id, 2) NOT GLOB '*[^0-9]*')
     OR (chat_relays.from_chat_id <> '' AND chat_relays.from_chat_id NOT GLOB '*[^0-9]*')
     OR (chat_relays.from_chat_id NOT GLOB '*:*:*' AND instr(chat_relays.from_chat_id, ':') > 1
           AND substr(chat_relays.from_chat_id, instr(chat_relays.from_chat_id, ':') + 1) <> ''
           AND substr(chat_relays.from_chat_id, instr(chat_relays.from_chat_id, ':') + 1) NOT GLOB '*[^0-9]*'
           AND ((substr(chat_relays.from_chat_id, 1, 1) = '-' AND instr(chat_relays.from_chat_id, ':') > 2
                   AND substr(chat_relays.from_chat_id, 2, instr(chat_relays.from_chat_id, ':') - 2) NOT GLOB '*[^0-9]*')
                OR substr(chat_relays.from_chat_id, 1, instr(chat_relays.from_chat_id, ':') - 1) NOT GLOB '*[^0-9]*'))
       )
   AND NOT EXISTS (SELECT 1 FROM chats WHERE chats.chat_id = chat_relays.from_chat_id);

UPDATE chat_relays SET to_chat_id = 'telegram:' || to_chat_id
 WHERE chat_relays.to_chat_id IS NOT NULL
   AND (
        (chat_relays.to_chat_id <> '' AND substr(chat_relays.to_chat_id, 1, 1) = '-' AND length(chat_relays.to_chat_id) > 1
           AND substr(chat_relays.to_chat_id, 2) NOT GLOB '*[^0-9]*')
     OR (chat_relays.to_chat_id <> '' AND chat_relays.to_chat_id NOT GLOB '*[^0-9]*')
     OR (chat_relays.to_chat_id NOT GLOB '*:*:*' AND instr(chat_relays.to_chat_id, ':') > 1
           AND substr(chat_relays.to_chat_id, instr(chat_relays.to_chat_id, ':') + 1) <> ''
           AND substr(chat_relays.to_chat_id, instr(chat_relays.to_chat_id, ':') + 1) NOT GLOB '*[^0-9]*'
           AND ((substr(chat_relays.to_chat_id, 1, 1) = '-' AND instr(chat_relays.to_chat_id, ':') > 2
                   AND substr(chat_relays.to_chat_id, 2, instr(chat_relays.to_chat_id, ':') - 2) NOT GLOB '*[^0-9]*')
                OR substr(chat_relays.to_chat_id, 1, instr(chat_relays.to_chat_id, ':') - 1) NOT GLOB '*[^0-9]*'))
       )
   AND NOT EXISTS (SELECT 1 FROM chats WHERE chats.chat_id = chat_relays.to_chat_id);

UPDATE chat_queue SET chat_id = 'telegram:' || chat_id
 WHERE chat_queue.chat_id IS NOT NULL
   AND (
        (chat_queue.chat_id <> '' AND substr(chat_queue.chat_id, 1, 1) = '-' AND length(chat_queue.chat_id) > 1
           AND substr(chat_queue.chat_id, 2) NOT GLOB '*[^0-9]*')
     OR (chat_queue.chat_id <> '' AND chat_queue.chat_id NOT GLOB '*[^0-9]*')
     OR (chat_queue.chat_id NOT GLOB '*:*:*' AND instr(chat_queue.chat_id, ':') > 1
           AND substr(chat_queue.chat_id, instr(chat_queue.chat_id, ':') + 1) <> ''
           AND substr(chat_queue.chat_id, instr(chat_queue.chat_id, ':') + 1) NOT GLOB '*[^0-9]*'
           AND ((substr(chat_queue.chat_id, 1, 1) = '-' AND instr(chat_queue.chat_id, ':') > 2
                   AND substr(chat_queue.chat_id, 2, instr(chat_queue.chat_id, ':') - 2) NOT GLOB '*[^0-9]*')
                OR substr(chat_queue.chat_id, 1, instr(chat_queue.chat_id, ':') - 1) NOT GLOB '*[^0-9]*'))
       )
   AND NOT EXISTS (SELECT 1 FROM chats WHERE chats.chat_id = chat_queue.chat_id);

UPDATE chat_said_now SET chat_id = 'telegram:' || chat_id
 WHERE chat_said_now.chat_id IS NOT NULL
   AND (
        (chat_said_now.chat_id <> '' AND substr(chat_said_now.chat_id, 1, 1) = '-' AND length(chat_said_now.chat_id) > 1
           AND substr(chat_said_now.chat_id, 2) NOT GLOB '*[^0-9]*')
     OR (chat_said_now.chat_id <> '' AND chat_said_now.chat_id NOT GLOB '*[^0-9]*')
     OR (chat_said_now.chat_id NOT GLOB '*:*:*' AND instr(chat_said_now.chat_id, ':') > 1
           AND substr(chat_said_now.chat_id, instr(chat_said_now.chat_id, ':') + 1) <> ''
           AND substr(chat_said_now.chat_id, instr(chat_said_now.chat_id, ':') + 1) NOT GLOB '*[^0-9]*'
           AND ((substr(chat_said_now.chat_id, 1, 1) = '-' AND instr(chat_said_now.chat_id, ':') > 2
                   AND substr(chat_said_now.chat_id, 2, instr(chat_said_now.chat_id, ':') - 2) NOT GLOB '*[^0-9]*')
                OR substr(chat_said_now.chat_id, 1, instr(chat_said_now.chat_id, ':') - 1) NOT GLOB '*[^0-9]*'))
       )
   AND NOT EXISTS (SELECT 1 FROM chats WHERE chats.chat_id = chat_said_now.chat_id);

UPDATE chat_tasks SET chat_id = 'telegram:' || chat_id
 WHERE chat_tasks.chat_id IS NOT NULL
   AND (
        (chat_tasks.chat_id <> '' AND substr(chat_tasks.chat_id, 1, 1) = '-' AND length(chat_tasks.chat_id) > 1
           AND substr(chat_tasks.chat_id, 2) NOT GLOB '*[^0-9]*')
     OR (chat_tasks.chat_id <> '' AND chat_tasks.chat_id NOT GLOB '*[^0-9]*')
     OR (chat_tasks.chat_id NOT GLOB '*:*:*' AND instr(chat_tasks.chat_id, ':') > 1
           AND substr(chat_tasks.chat_id, instr(chat_tasks.chat_id, ':') + 1) <> ''
           AND substr(chat_tasks.chat_id, instr(chat_tasks.chat_id, ':') + 1) NOT GLOB '*[^0-9]*'
           AND ((substr(chat_tasks.chat_id, 1, 1) = '-' AND instr(chat_tasks.chat_id, ':') > 2
                   AND substr(chat_tasks.chat_id, 2, instr(chat_tasks.chat_id, ':') - 2) NOT GLOB '*[^0-9]*')
                OR substr(chat_tasks.chat_id, 1, instr(chat_tasks.chat_id, ':') - 1) NOT GLOB '*[^0-9]*'))
       )
   AND NOT EXISTS (SELECT 1 FROM chats WHERE chats.chat_id = chat_tasks.chat_id);

UPDATE browser_sessions SET owner_chat_id = 'telegram:' || owner_chat_id
 WHERE browser_sessions.owner_chat_id IS NOT NULL
   AND (
        (browser_sessions.owner_chat_id <> '' AND substr(browser_sessions.owner_chat_id, 1, 1) = '-' AND length(browser_sessions.owner_chat_id) > 1
           AND substr(browser_sessions.owner_chat_id, 2) NOT GLOB '*[^0-9]*')
     OR (browser_sessions.owner_chat_id <> '' AND browser_sessions.owner_chat_id NOT GLOB '*[^0-9]*')
     OR (browser_sessions.owner_chat_id NOT GLOB '*:*:*' AND instr(browser_sessions.owner_chat_id, ':') > 1
           AND substr(browser_sessions.owner_chat_id, instr(browser_sessions.owner_chat_id, ':') + 1) <> ''
           AND substr(browser_sessions.owner_chat_id, instr(browser_sessions.owner_chat_id, ':') + 1) NOT GLOB '*[^0-9]*'
           AND ((substr(browser_sessions.owner_chat_id, 1, 1) = '-' AND instr(browser_sessions.owner_chat_id, ':') > 2
                   AND substr(browser_sessions.owner_chat_id, 2, instr(browser_sessions.owner_chat_id, ':') - 2) NOT GLOB '*[^0-9]*')
                OR substr(browser_sessions.owner_chat_id, 1, instr(browser_sessions.owner_chat_id, ':') - 1) NOT GLOB '*[^0-9]*'))
       )
   AND NOT EXISTS (SELECT 1 FROM chats WHERE chats.chat_id = browser_sessions.owner_chat_id);
