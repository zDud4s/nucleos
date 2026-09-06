-- O sexto degrau da escada: `dont_ask`.
--
-- `0129_permission_mode.sql` escreveu o CHECK de `chats.permission_mode` com cinco grafias, e o
-- cabeçalho dessa migração diz porquê: «O CHECK é agora ou nunca. Acrescentar um mais tarde
-- significa RECONSTRUIR `chats`». Chegou o mais tarde. O SQLite não sabe alargar um CHECK, por isso
-- alargar o conjunto de degraus que uma conversa pode nomear é reconstruir a tabela — a mesma
-- operação que `0123_brain_openrouter.sql` fez para alargar `brain`, e é a forma dessa migração que
-- esta copia.
--
-- O que o degrau novo faz, para quem ler isto sem o código à frente: permite exactamente o que
-- `auto` permite, e RECUSA tudo aquilo sobre que `auto` teria parado para perguntar. É uma conversa
-- que não pergunta a ninguém. Não termina o turno e não esvazia fila nenhuma — recusa uma chamada
-- de ferramenta, e o turno continua.
--
-- A lista de colunas abaixo é o ESQUEMA TAL COMO ESTÁ AGORA, lido de `PRAGMA table_info(chats)`
-- numa base acabada de migrar, e não a lista de nenhuma migração anterior. São 22 colunas: as 21
-- de `0123` mais `permission_mode`, que `0129` acrescentou depois. Escrever esta lista de memória,
-- ou a partir da migração que se encontra primeiro com `CREATE TABLE chats` lá dentro, é o erro
-- que o cabeçalho de `0123` documenta ter já acontecido aqui uma vez — e o custo é deixar cair
-- colunas em silêncio, com os dados de toda a gente dentro delas.
--
-- Sem `-- no-transaction` e sem `PRAGMA foreign_keys`, como em `0123` e pela mesma razão: nada
-- neste esquema declara `REFERENCES chats`, portanto não há chave estrangeira para o pragma
-- proteger nem para se perder no DROP. Pegar num deles numa tabela para a qual ninguém aponta é
-- como uma reconstrução segura se torna perigosa.
--
-- `runs.permission_mode` NÃO é tocada aqui, e a assimetria é deliberada: `0129:34` deixou-a sem
-- CHECK porque é um INSTANTÂNEO do modo com que o turno começou, e um instantâneo de uma grafia
-- que já não escrevemos tem de continuar a poder ser lido.
CREATE TABLE chats_new (
  chat_id              TEXT PRIMARY KEY,
  title                TEXT,
  brain                TEXT NOT NULL DEFAULT 'cloud' CHECK (brain IN ('cloud', 'local', 'openrouter')),
  created_at           TEXT NOT NULL,
  archived_at          TEXT,
  last_seen_turn_id    INTEGER,
  cwd                  TEXT,
  ide_session_id       TEXT,
  handover             TEXT,
  plan_only            INTEGER NOT NULL DEFAULT 0,
  model                TEXT,
  effort               TEXT,
  extra_dirs           TEXT,
  turn_budget_usd      REAL,
  fallback_model       TEXT,
  agents               TEXT,
  system_prompt        TEXT,
  denied_tools         TEXT,
  cleared_after_run_id INTEGER,
  context_window       INTEGER,
  last_seen_notice_id  INTEGER,
  permission_mode      TEXT NOT NULL DEFAULT 'auto' CHECK (permission_mode IN ('manual', 'accept_edits', 'plan', 'auto', 'bypass', 'dont_ask'))
);

-- Todas as colunas nomeadas uma a uma, e nunca `SELECT *`: a ordem de `SELECT *` é a da tabela
-- antiga, e basta uma migração futura acrescentar uma coluna a meio desta lista para o `INSERT`
-- passar a encher a coluna errada sem se queixar de nada.
INSERT INTO chats_new
SELECT chat_id, title, brain, created_at, archived_at, last_seen_turn_id, cwd, ide_session_id,
       handover, plan_only, model, effort, extra_dirs, turn_budget_usd, fallback_model, agents,
       system_prompt, denied_tools, cleared_after_run_id, context_window, last_seen_notice_id,
       permission_mode
  FROM chats;

DROP TABLE chats;
ALTER TABLE chats_new RENAME TO chats;

-- Recriado porque a tabela que indexava deixou de existir. Igual ao que substitui, de propósito —
-- esta migração alarga `permission_mode` e mais nada, e um índice que aqui mudasse de forma em
-- silêncio seria a alteração mais difícil de encontrar depois.
CREATE INDEX chats_by_activity ON chats (archived_at, created_at);
